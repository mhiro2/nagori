use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a "last pasted" entry id stays valid before it falls back to
/// the recency head. Picked at 30 min so a short break between pastes
/// (coffee, meeting) still threads back to the same clip, but a fresh
/// session many hours later doesn't surface a context-mismatched paste
/// from a different task.
const LAST_PASTED_TTL: Duration = Duration::from_mins(30);

use nagori_core::{AppError, AppSettings, EntryId, Result};
use nagori_daemon::{
    CaptureLoop, MaintenanceHealth, MaintenanceReport, MaintenanceService, NagoriRuntime,
    StartupHealth,
};
use nagori_platform_native::{NativeRuntimeOptions, build_native_runtime};
use nagori_storage::SqliteStore;

use nagori_platform::{ClipboardReader, PreviewController, RestoreTarget, WindowBehavior};
#[cfg(target_os = "macos")]
use nagori_platform_macos::MacosWindowBehavior;
#[cfg(target_os = "windows")]
use nagori_platform_windows::WindowsWindowBehavior;

pub struct AppState {
    pub runtime: NagoriRuntime,
    pub window: Arc<dyn WindowBehavior>,
    /// OS-native preview surface (Quick Look on macOS, `Unsupported`
    /// stub on Windows / Linux). The Tauri `preview_entry` command
    /// drives this directly from the desktop process rather than going
    /// through IPC — the daemon does not run an `AppKit` event loop, so
    /// the macOS adapter would not work from a free-standing daemon
    /// even if we wired it. The capability layer reports `Unsupported`
    /// on the OSes where this stub is wired, so palette UI can suppress
    /// the shortcut up front.
    pub preview: Arc<dyn PreviewController>,
    /// Clipboard reader handle shared with the runtime's writer. Holding the
    /// same `Arc` on both sides means the capture loop and the paste/copy path
    /// can't drift into a state where one is wired to a working adapter and
    /// the other to a stub: any platform-init failure surfaces once in
    /// `try_new_at` (with Wayland guidance on Linux) and aborts startup,
    /// rather than letting the app come up with the writer healthy and a
    /// silently-dead capture task.
    capture_reader: Arc<dyn ClipboardReader>,
    background_tasks: Mutex<Option<BackgroundTasks>>,
    /// Frontmost app captured the last time the palette was opened.
    /// Used by `paste_entry` to re-focus the user's prior app before
    /// synthesising ⌘V — without it, the keystroke lands on the
    /// (still-focused) Nagori webview and we paste into our own search
    /// box. On Linux Wayland the snapshot is always `None` (the
    /// compositor refuses to expose a portable frontmost-app query), so
    /// the palette skips the refocus step and relies on `wtype` to
    /// target whatever the compositor considers focused after our window
    /// hides. On Windows the snapshot now carries the foreground HWND
    /// in `native_handle`, so `activate_restore_target` can re-foreground
    /// the original window via `SetForegroundWindow`.
    pub previous_frontmost: Arc<Mutex<Option<RestoreTarget>>>,
    /// Most recently pasted entry id, paired with the `Instant` it was
    /// recorded. Powers the "repaste last" secondary hotkey so it
    /// targets the entry the user actually pasted instead of whatever
    /// happens to top the recency list (a fresh capture from elsewhere
    /// can otherwise hijack the slot between pastes). The timestamp
    /// drives TTL expiry — see `LAST_PASTED_TTL` — so a paste recorded
    /// hours ago doesn't silently resurface in a new working context.
    pub last_pasted_id: Mutex<Option<(EntryId, Instant)>>,
    /// Latest global-hotkey registration failures, split per `kind`.
    /// Persisted so the always-on App-level subscriber can re-hydrate
    /// the toast/banner after a window opens past the live emit (startup
    /// race) or after `SettingsView` is re-mounted later. A primary and
    /// a secondary failure are tracked independently so a primary
    /// success (or vice versa) doesn't silently wipe an unresolved
    /// failure on the other side. The frontend reads via
    /// `last_hotkey_failure` and subscribes to the live
    /// `nagori://hotkey_register_failed` / `_resolved` events for
    /// updates.
    pub last_hotkey_failure: Mutex<HotkeyFailureCache>,
}

/// Snapshot of a global-hotkey registration failure shared across the
/// emit (`nagori://hotkey_register_failed`) and the cached state queried
/// by `last_hotkey_failure`. Kept in `state.rs` so it can be referenced
/// from both `lib.rs` (emit site) and `commands.rs` (query command)
/// without dragging the kind enum across module boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyFailureRecord {
    pub hotkey: String,
    pub error: String,
    /// `Some("secondary")` for secondary accelerators, `None` for the
    /// primary palette shortcut — mirrors the wire shape emitted on
    /// `nagori://hotkey_register_failed`.
    pub kind: Option<String>,
    /// Identifier of the secondary action whose register failed (the
    /// kebab-case wire value mirroring `SecondaryHotkeyAction`'s serde
    /// representation). `None` for primary failures, where the binding
    /// is global. Carried on the wire too: the frontend's single-slot
    /// store uses it to discriminate "this exact action resolved" from
    /// "a sibling action sharing the accelerator resolved", so a
    /// secondary resolve cannot wipe an unrelated still-failing
    /// secondary.
    pub action: Option<String>,
}

/// Cache of hotkey registration failures keyed for independent
/// resolution. The primary slot is single-slot (there is only one
/// palette shortcut), but secondaries are keyed by *action* — two
/// secondary actions can fail at the same time (or share the same
/// accelerator), and resolving one must not silently lose the other
/// from cache + hydration. The action key is the kebab-case wire value
/// (`repaste-last`, `clear-history`); cached secondary records without
/// an action identifier are skipped on insert because there would be
/// no way to address them on a later resolve.
#[derive(Debug, Default, Clone)]
pub struct HotkeyFailureCache {
    pub primary: Option<HotkeyFailureRecord>,
    pub secondary: BTreeMap<String, HotkeyFailureRecord>,
}

impl HotkeyFailureCache {
    /// Route a new failure to the matching slot. Primary records (and
    /// any record missing `kind`) land in the single primary slot; a
    /// secondary record is keyed by its `action` wire value. A
    /// secondary record without an action identifier is dropped — the
    /// per-action cache has no way to address it on a later resolve,
    /// so caching it would only produce permanently-stuck entries.
    pub fn record(&mut self, record: HotkeyFailureRecord) {
        match record.kind.as_deref() {
            Some("secondary") => {
                if let Some(action) = record.action.clone() {
                    self.secondary.insert(action, record);
                }
            }
            _ => self.primary = Some(record),
        }
    }

    /// Clear the slot identified by `kind` (+ `action` for secondaries).
    /// Returns whether anything was actually cleared so the caller can
    /// skip emitting a paired resolved event when the cache was empty.
    /// A secondary clear without an action is a no-op — a blanket clear
    /// would wipe sibling actions that share the kind, which is exactly
    /// the bug the per-action cache exists to prevent.
    pub fn clear_for_kind_action(&mut self, kind: Option<&str>, action: Option<&str>) -> bool {
        match (kind, action) {
            (None, _) => self.primary.take().is_some(),
            (Some("secondary"), Some(a)) => self.secondary.remove(a).is_some(),
            _ => false,
        }
    }

    /// Most-relevant cached failure for a single-slot consumer. Primary
    /// wins over secondary — the palette toggle being broken is
    /// strictly more disruptive than a missing secondary action. With
    /// no primary failure, returns an arbitrary-but-deterministic
    /// secondary (`BTreeMap` first entry — alphabetical by action wire
    /// value).
    pub fn most_relevant(&self) -> Option<&HotkeyFailureRecord> {
        self.primary
            .as_ref()
            .or_else(|| self.secondary.values().next())
    }
}

struct BackgroundTasks {
    capture: tauri::async_runtime::JoinHandle<()>,
    maintenance: tauri::async_runtime::JoinHandle<()>,
    semantic: tauri::async_runtime::JoinHandle<()>,
    ngram_rebuild: tauri::async_runtime::JoinHandle<()>,
}

impl AppState {
    pub fn record_last_pasted(&self, id: EntryId) {
        if let Ok(mut slot) = self.last_pasted_id.lock() {
            *slot = Some((id, Instant::now()));
        }
    }

    pub fn last_pasted(&self) -> Option<EntryId> {
        let mut slot = self.last_pasted_id.lock().ok()?;
        let (id, recorded_at) = (*slot)?;
        if recorded_at.elapsed() >= LAST_PASTED_TTL {
            // Expired entries are cleared on read so a stale id can't be
            // picked up by a later mutation path that compares against
            // `slot.id` (e.g. `clear_last_pasted_if`).
            *slot = None;
            return None;
        }
        Some(id)
    }

    /// Clear the last-pasted slot if it currently holds `id`. Called after
    /// any path that removes the entry (single delete, bulk delete) so the
    /// next "repaste last" falls through to the recency fallback rather
    /// than failing with `NotFound`.
    pub fn clear_last_pasted_if(&self, id: EntryId) {
        if let Ok(mut slot) = self.last_pasted_id.lock()
            && let Some((stored_id, _)) = *slot
            && stored_id == id
        {
            *slot = None;
        }
    }

    /// Clear the last-pasted slot unconditionally. Used by `clear_history`
    /// and other bulk-purge paths where any tracked id is presumed gone.
    pub fn clear_last_pasted(&self) {
        if let Ok(mut slot) = self.last_pasted_id.lock() {
            *slot = None;
        }
    }

    /// Paste the tracked last-pasted entry, falling back to the recency
    /// head when none is tracked or the tracked id has been retention-swept.
    /// Returns `AppError::NotFound` when neither path has anything to paste
    /// (no last-pasted slot and an empty history).
    pub async fn repaste_last_or_recency(&self) -> Result<()> {
        if let Some(id) = self.last_pasted() {
            match self.runtime.paste_entry(id, None).await {
                Ok(()) => {
                    self.record_last_pasted(id);
                    return Ok(());
                }
                Err(AppError::NotFound) => self.clear_last_pasted_if(id),
                Err(err) => return Err(err),
            }
        }
        let entries = self.runtime.list_recent(1).await?;
        let Some(entry) = entries.into_iter().next() else {
            return Err(AppError::NotFound);
        };
        self.runtime.paste_entry(entry.id, None).await?;
        self.record_last_pasted(entry.id);
        Ok(())
    }
}

impl AppState {
    /// Snapshot the current frontmost app and store it as the "previous
    /// frontmost" — call this immediately *before* showing the palette so
    /// the snapshot reflects the source the user copied from / wants to
    /// paste back into. macOS uses `AppKit`, Windows uses
    /// `GetForegroundWindow` (and stamps the HWND into `native_handle`
    /// so `SetForegroundWindow` can re-foreground the *original* window
    /// at paste time), Linux Wayland records `None` because the
    /// compositor does not expose a portable foreground-surface query.
    pub fn remember_previous_frontmost(&self) {
        let snapshot = capture_restore_target_blocking();
        if let Ok(mut slot) = self.previous_frontmost.lock() {
            *slot = snapshot;
        }
    }

    pub fn take_previous_frontmost(&self) -> Option<RestoreTarget> {
        self.previous_frontmost
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
    }

    pub fn clear_previous_frontmost(&self) {
        if let Ok(mut slot) = self.previous_frontmost.lock() {
            *slot = None;
        }
    }
}

/// Cross-platform synchronous restore-target probe used to seed
/// `previous_frontmost`. The helper avoids dragging a `tokio` runtime
/// into Tauri command callbacks (some are sync, e.g. `open_palette`) by
/// going through each platform crate's `_blocking` accessor. Linux
/// Wayland has no portable equivalent, so the helper returns `None`
/// without erroring — see `LinuxWindowBehavior` for the trade-off.
#[cfg(target_os = "macos")]
fn capture_restore_target_blocking() -> Option<RestoreTarget> {
    MacosWindowBehavior::capture_restore_target_blocking()
}

#[cfg(target_os = "windows")]
fn capture_restore_target_blocking() -> Option<RestoreTarget> {
    WindowsWindowBehavior::capture_restore_target_blocking()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const fn capture_restore_target_blocking() -> Option<RestoreTarget> {
    None
}

impl AppState {
    pub fn try_new() -> Result<Self> {
        let db_path = default_db_path();
        Self::try_new_at(&db_path)
    }

    /// Open the store at `db_path` and wrap any failure with that path and
    /// recovery guidance. The setup closure prints these errors directly to
    /// the user, so the message must be self-explanatory: which file failed,
    /// which permission was needed, and what command will move the broken
    /// DB aside without losing data.
    pub fn try_new_at(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            nagori_storage::ensure_private_directory(parent)
                .map_err(|err| annotate_startup_error(&err, db_path, StartupStage::Directory))?;
        }
        let store = SqliteStore::open(db_path)
            .map_err(|err| annotate_startup_error(&err, db_path, StartupStage::OpenDb))?;
        Self::build(store)
    }

    /// Spawns the in-process capture loop and a low-frequency maintenance
    /// loop. Call once after `manage(state)` so a Tokio runtime is available.
    pub fn spawn_background_tasks(&self, app: tauri::AppHandle) {
        let mut tasks_slot = self.background_tasks_slot();
        if tasks_slot.is_some() {
            tracing::warn!("background_tasks_already_started");
            return;
        }

        let runtime = self.runtime.clone();
        let window = self.window.clone();
        let reader = self.capture_reader.clone();
        let search_cache = self.runtime.search_cache_handle();
        let startup_health = self.runtime.startup_health();
        let capture_health = self.runtime.capture_health();
        let capture = tauri::async_runtime::spawn(async move {
            // Fail closed: refuse to start the capture loop if the persisted
            // settings cannot be loaded — running with `Default` would drop
            // the user's denylist / regex_denylist / secret_handling and
            // capture more aggressively than configured.
            let refresh = runtime.refresh_settings_from_store().await;
            if !note_capture_settings_load_outcome(&startup_health, &refresh) {
                return;
            }
            let store = runtime.store().clone();
            let settings = runtime.current_settings();
            let app_for_capture_event = app.clone();
            let runtime_for_notify = runtime.clone();
            let capture_notifier = Arc::new(move |entry_id: EntryId| {
                use tauri::Emitter;

                let _ = app_for_capture_event.emit(
                    crate::CLIPBOARD_CHANGED_EVENT,
                    serde_json::json!({ "entryId": entry_id.to_string() }),
                );
                // Nudge the semantic indexer so the fresh clip is embedded
                // promptly (no-op when the index is disabled / unsupported).
                runtime_for_notify.notify_semantic_capture();
            });
            let mut capture = CaptureLoop::new(reader, store.clone(), store.clone(), settings)
                .with_window(window)
                .with_search_cache(search_cache)
                .with_capture_health(capture_health)
                .with_capture_notifier(capture_notifier);
            let mut shutdown = runtime.shutdown_handle();
            let shutdown_signal = async move { shutdown.cancelled().await };
            if let Err(err) = capture
                .run_polling_with_settings(
                    Duration::from_millis(500),
                    runtime.settings_subscribe(),
                    shutdown_signal,
                )
                .await
            {
                tracing::warn!(error = %err, "capture_loop_terminated");
            }
        });

        let runtime = self.runtime.clone();
        let maintenance = tauri::async_runtime::spawn(async move {
            let store = runtime.store().clone();
            let mut settings_rx = runtime.settings_subscribe();
            let mut shutdown = runtime.shutdown_handle();
            let health = runtime.maintenance_health();
            let maintenance =
                MaintenanceService::new(store).with_search_cache(runtime.search_cache_handle());
            loop {
                let settings = settings_rx.borrow().clone();
                let outcome = maintenance.run(&settings).await;
                note_maintenance_outcome(&health, &outcome);
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    _ = settings_rx.changed() => {},
                    () = tokio::time::sleep(Duration::from_mins(30)) => {},
                }
            }
        });

        let runtime = self.runtime.clone();
        let semantic = tauri::async_runtime::spawn(async move {
            let shutdown = runtime.shutdown_handle();
            runtime.run_semantic_indexer(shutdown).await;
        });

        // One-shot backfill of ngrams left stale by a generator upgrade (kana
        // folding / Han 1-grams). The desktop app drives `NagoriRuntime`
        // directly without the CLI daemon's serve loop, so it must spawn this
        // worker itself — otherwise a desktop-only history never gets its old
        // rows rebuilt and CJK search improvements don't apply to them.
        let runtime = self.runtime.clone();
        let ngram_rebuild = tauri::async_runtime::spawn(async move {
            let shutdown = runtime.shutdown_handle();
            runtime.run_ngram_rebuild(shutdown).await;
        });

        *tasks_slot = Some(BackgroundTasks {
            capture,
            maintenance,
            semantic,
            ngram_rebuild,
        });
    }

    /// Cancel, drain, and abort the in-process capture and maintenance
    /// workers. Safe to call more than once; only the first call owns the
    /// task handles.
    pub async fn shutdown_background_tasks(&self, grace: Duration) {
        self.runtime.shutdown_handle().cancel();
        let Some(tasks) = self.background_tasks_slot().take() else {
            return;
        };
        tokio::join!(
            drain_background_task("capture", tasks.capture, grace),
            drain_background_task("maintenance", tasks.maintenance, grace),
            drain_background_task("semantic", tasks.semantic, grace),
            drain_background_task("ngram_rebuild", tasks.ngram_rebuild, grace),
        );
    }

    fn background_tasks_slot(&self) -> std::sync::MutexGuard<'_, Option<BackgroundTasks>> {
        self.background_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn build(store: SqliteStore) -> Result<Self> {
        let parts = build_native_runtime(store, NativeRuntimeOptions::default())?;
        Ok(Self {
            runtime: parts.runtime,
            window: parts.window,
            preview: parts.preview,
            capture_reader: parts.clipboard_reader,
            background_tasks: Mutex::new(None),
            previous_frontmost: Arc::new(Mutex::new(None)),
            last_pasted_id: Mutex::new(None),
            last_hotkey_failure: Mutex::new(HotkeyFailureCache::default()),
        })
    }

    /// Record a hotkey registration failure so a later-mounted listener
    /// can re-hydrate it. Primary lands in the single slot; secondaries
    /// are keyed by action wire value so two simultaneously-failing
    /// secondaries don't overwrite each other.
    pub fn record_hotkey_failure(&self, record: HotkeyFailureRecord) {
        if let Ok(mut cache) = self.last_hotkey_failure.lock() {
            cache.record(record);
        }
    }

    /// Clear the cached hotkey failure for the slot matching
    /// `(kind, action)`. Returns `true` if a record was actually cleared
    /// so the caller can emit a paired resolved event without a redundant
    /// poll. `(None, _)` clears the primary slot; `(Some("secondary"),
    /// Some(action))` clears that exact secondary entry. A primary
    /// success cannot wipe any secondary (and vice versa), and a
    /// secondary success only wipes its own action's entry — sibling
    /// actions sharing the kind keep their cached failures.
    pub fn clear_hotkey_failure_for_kind_action(
        &self,
        kind: Option<&str>,
        action: Option<&str>,
    ) -> bool {
        match self.last_hotkey_failure.lock() {
            Ok(mut cache) => cache.clear_for_kind_action(kind, action),
            Err(_) => false,
        }
    }

    /// Read the most-relevant cached hotkey failure for hydration on a
    /// late-mounted listener. The frontend store is single-slot, so
    /// when both kinds are failing simultaneously we prioritise
    /// primary — the palette toggle being broken is strictly more
    /// disruptive than a missing secondary action.
    pub fn current_hotkey_failure(&self) -> Option<HotkeyFailureRecord> {
        let cache = self.last_hotkey_failure.lock().ok()?;
        cache.most_relevant().cloned()
    }

    /// Snapshot the full cache (both kinds) so callers reconciling
    /// against current settings can decide which slots are now stale
    /// without holding the mutex across the comparison.
    pub fn hotkey_failure_cache_snapshot(&self) -> HotkeyFailureCache {
        self.last_hotkey_failure
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }
}

async fn drain_background_task(
    name: &'static str,
    mut handle: tauri::async_runtime::JoinHandle<()>,
    grace: Duration,
) {
    match tokio::time::timeout(grace, &mut handle).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => tracing::warn!(error = %err, worker = name, "background_task_join_failed"),
        Err(_) => {
            tracing::warn!(worker = name, "background_task_drain_timeout_aborting");
            handle.abort();
            match handle.await {
                Ok(()) => {}
                Err(tauri::Error::JoinError(err)) if err.is_cancelled() => {}
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        worker = name,
                        "background_task_abort_join_failed"
                    );
                }
            }
        }
    }
}

/// Funnels the capture task's `refresh_settings_from_store` outcome into the
/// shared `StartupHealth` signal and decides whether the capture loop should
/// proceed to enter polling. Extracted so the wiring between "settings load
/// failed" and "`StartupHealth` records failed" can be pinned by a unit test
/// rather than living only inside `tauri::async_runtime::spawn`, where the
/// previous inline version silently dropped failures and left users with a
/// "Clipboard history is ready" notification while capture never started.
pub(crate) fn note_capture_settings_load_outcome(
    health: &StartupHealth,
    result: &Result<AppSettings>,
) -> bool {
    match result {
        Ok(_) => {
            health.record_capture_ready();
            true
        }
        Err(err) => {
            health.record_capture_failed(err.to_string());
            tracing::error!(error = %err, "settings_load_failed_aborting_capture");
            false
        }
    }
}

/// Subscriber-side counterpart to `note_capture_settings_load_outcome`. The
/// settings subscriber and the capture task both abort on a failed initial
/// settings load; recording the same failure here means a subscriber-only
/// abort (which the capture task may not even reach) still flips the
/// desktop's gated "ready" notification to "failed". `StartupHealth` is
/// first-outcome-wins, so calling this after a capture-side success cannot
/// mask the running state.
pub(crate) fn record_subscriber_settings_load_failure(health: &StartupHealth, err: &AppError) {
    health.record_capture_failed(err.to_string());
    tracing::error!(error = %err, "settings_load_failed_aborting_subscribers");
}

/// Funnels one maintenance iteration's outcome into `MaintenanceHealth` so
/// `nagori doctor` reflects retention failures on the desktop the same way
/// it does on the daemon (`serve.rs`). Extracted from the spawn body so the
/// "did the desktop record the outcome?" contract is pinned by a unit test
/// instead of living inside `tauri::async_runtime::spawn`, where the prior
/// inline version dropped maintenance results on the floor and let `nagori
/// doctor` report `consecutive_failures=0` against a wedged loop.
pub(crate) fn note_maintenance_outcome(
    health: &MaintenanceHealth,
    result: &Result<MaintenanceReport>,
) {
    match result {
        Ok(_) => health.record_success(),
        Err(err) => {
            health.record_failure(err.to_string());
            tracing::warn!(error = %err, "maintenance_failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use super::*;

    fn primary_record(hotkey: &str) -> HotkeyFailureRecord {
        HotkeyFailureRecord {
            hotkey: hotkey.to_owned(),
            error: "boom".to_owned(),
            kind: None,
            action: None,
        }
    }

    fn secondary_record(hotkey: &str, action: &str) -> HotkeyFailureRecord {
        HotkeyFailureRecord {
            hotkey: hotkey.to_owned(),
            error: "boom".to_owned(),
            kind: Some("secondary".to_owned()),
            action: Some(action.to_owned()),
        }
    }

    #[test]
    fn hotkey_cache_keeps_primary_and_secondary_independently() {
        // A secondary failure cached first must survive a primary
        // failure landing next — a single-slot cache would silently
        // overwrite it, hiding an unresolved binding from any later
        // hydration.
        let mut cache = HotkeyFailureCache::default();
        cache.record(secondary_record("Cmd+Shift+R", "repaste-last"));
        cache.record(primary_record("Cmd+Shift+V"));
        assert_eq!(
            cache.primary.as_ref().map(|r| r.hotkey.as_str()),
            Some("Cmd+Shift+V")
        );
        assert_eq!(
            cache
                .secondary
                .get("repaste-last")
                .map(|r| r.hotkey.as_str()),
            Some("Cmd+Shift+R")
        );
    }

    #[test]
    fn hotkey_cache_keeps_two_secondaries_independently() {
        // Two secondary actions failing simultaneously is the bug a
        // single-slot secondary cache silently hides: the later record
        // overwrites the first and the user only ever sees one banner,
        // with the other failure permanently absent from hydration.
        let mut cache = HotkeyFailureCache::default();
        cache.record(secondary_record("Cmd+Shift+R", "repaste-last"));
        cache.record(secondary_record("Cmd+Shift+K", "clear-history"));
        assert_eq!(
            cache
                .secondary
                .get("repaste-last")
                .map(|r| r.hotkey.as_str()),
            Some("Cmd+Shift+R")
        );
        assert_eq!(
            cache
                .secondary
                .get("clear-history")
                .map(|r| r.hotkey.as_str()),
            Some("Cmd+Shift+K")
        );
    }

    #[test]
    fn hotkey_cache_drops_secondary_record_missing_action() {
        // The per-action map needs an addressable key. A secondary
        // record with no action identifier would be permanently
        // unclearable, so we drop it on insert rather than caching a
        // stuck entry. This shape is not produced by current emit
        // sites but guards against future regressions.
        let mut cache = HotkeyFailureCache::default();
        cache.record(HotkeyFailureRecord {
            hotkey: "Cmd+Shift+R".to_owned(),
            error: "boom".to_owned(),
            kind: Some("secondary".to_owned()),
            action: None,
        });
        assert!(cache.secondary.is_empty());
    }

    #[test]
    fn hotkey_cache_clear_for_kind_action_only_touches_matching_slot() {
        // Clearing primary must leave secondary alone (and vice versa)
        // so a primary success cannot silently wipe a still-failing
        // secondary from cache + hydration. Likewise, clearing one
        // secondary action must not affect a sibling action's entry.
        let mut cache = HotkeyFailureCache::default();
        cache.record(primary_record("Cmd+Shift+V"));
        cache.record(secondary_record("Cmd+Shift+R", "repaste-last"));
        cache.record(secondary_record("Cmd+Shift+K", "clear-history"));

        assert!(cache.clear_for_kind_action(None, None));
        assert!(cache.primary.is_none());
        assert_eq!(
            cache
                .secondary
                .get("repaste-last")
                .map(|r| r.hotkey.as_str()),
            Some("Cmd+Shift+R")
        );
        assert_eq!(
            cache
                .secondary
                .get("clear-history")
                .map(|r| r.hotkey.as_str()),
            Some("Cmd+Shift+K")
        );

        // Second primary clear is a no-op (already empty) — caller
        // uses this to decide whether to emit a resolved event.
        assert!(!cache.clear_for_kind_action(None, None));

        assert!(cache.clear_for_kind_action(Some("secondary"), Some("repaste-last")));
        assert!(!cache.secondary.contains_key("repaste-last"));
        assert_eq!(
            cache
                .secondary
                .get("clear-history")
                .map(|r| r.hotkey.as_str()),
            Some("Cmd+Shift+K")
        );
        assert!(!cache.clear_for_kind_action(Some("secondary"), Some("repaste-last")));

        // A blanket secondary clear (no action) must be a no-op — the
        // bug we are guarding against.
        assert!(!cache.clear_for_kind_action(Some("secondary"), None));
        assert!(cache.secondary.contains_key("clear-history"));
    }

    #[test]
    fn hotkey_cache_most_relevant_prefers_primary() {
        // The hydration path returns a single failure to a single-slot
        // frontend store. Primary takes priority because the palette
        // toggle being broken is strictly more disruptive than a
        // missing secondary action.
        let mut cache = HotkeyFailureCache::default();
        assert!(cache.most_relevant().is_none());

        cache.record(secondary_record("Cmd+Shift+R", "repaste-last"));
        assert_eq!(
            cache.most_relevant().map(|r| r.hotkey.as_str()),
            Some("Cmd+Shift+R")
        );

        cache.record(primary_record("Cmd+Shift+V"));
        assert_eq!(
            cache.most_relevant().map(|r| r.hotkey.as_str()),
            Some("Cmd+Shift+V")
        );

        // After primary resolves, hydration falls through to a still
        // unresolved secondary — the failure isn't lost to the next
        // window mount.
        assert!(cache.clear_for_kind_action(None, None));
        assert_eq!(
            cache.most_relevant().map(|r| r.hotkey.as_str()),
            Some("Cmd+Shift+R")
        );
    }

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn drain_background_task_aborts_after_timeout() {
        let dropped = Arc::new(AtomicBool::new(false));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let task_dropped = dropped.clone();
        let handle = tauri::async_runtime::spawn(async move {
            let _guard = DropFlag(task_dropped);
            started_tx.send(()).expect("start signal should send");
            future::pending::<()>().await;
        });

        started_rx.await.expect("task should start");
        drain_background_task("test", handle, Duration::from_millis(10)).await;

        assert!(dropped.load(Ordering::SeqCst));
    }

    /// Capture-task abort path: when `refresh_settings_from_store` returns
    /// an error, `StartupHealth` must flip to `failed` with the error
    /// string preserved verbatim. This pins the wiring extracted out of
    /// `spawn_background_tasks` so a future inline refactor that drops
    /// the recording is caught even without running the full spawn.
    #[test]
    fn note_capture_settings_load_outcome_records_failure() {
        let health = StartupHealth::new();
        let err = AppError::Storage("disk full".to_owned());
        let expected = err.to_string();
        let result: Result<AppSettings> = Err(err);
        let proceed = note_capture_settings_load_outcome(&health, &result);
        assert!(!proceed, "capture loop must abort when settings load fails");
        let report = health.report();
        assert!(!report.ready);
        assert_eq!(report.last_error.as_deref(), Some(expected.as_str()));
    }

    /// Capture-task success path: a settled refresh must flip ready, with
    /// no error recorded. Combined with the failure test, this fixes the
    /// helper as the single source of truth for "did the capture loop
    /// reach polling?" — the bug that motivated 1.2 was precisely that
    /// the desktop notification fired before this signal existed.
    #[test]
    fn note_capture_settings_load_outcome_records_ready_on_success() {
        let health = StartupHealth::new();
        let result: Result<AppSettings> = Ok(AppSettings::default());
        let proceed = note_capture_settings_load_outcome(&health, &result);
        assert!(proceed, "capture loop must continue when settings load");
        let report = health.report();
        assert!(report.ready);
        assert!(report.last_error.is_none());
    }

    /// Subscriber-task abort path: like the capture-side helper but
    /// driven by the settings subscriber's own initial `get_settings`
    /// call. The subscriber and capture task race on the same store; the
    /// first abort to land wins. This test pins that the subscriber
    /// helper records the failure string as-is.
    #[test]
    fn record_subscriber_settings_load_failure_records_error_string() {
        let health = StartupHealth::new();
        let err = AppError::Storage("permission denied".to_owned());
        let expected = err.to_string();
        record_subscriber_settings_load_failure(&health, &err);
        let report = health.report();
        assert!(!report.ready);
        assert_eq!(report.last_error.as_deref(), Some(expected.as_str()));
    }

    /// First-outcome-wins must hold across both spawn helpers: a
    /// capture-side ready followed by a subscriber-side abort cannot
    /// downgrade an already-ready signal. This is critical for the
    /// desktop notification — once "Nagori is running" has fired, a
    /// late subscriber failure must not retroactively rewrite history.
    #[test]
    fn helpers_respect_first_outcome_wins() {
        let health = StartupHealth::new();
        assert!(note_capture_settings_load_outcome(
            &health,
            &Ok(AppSettings::default()),
        ));
        record_subscriber_settings_load_failure(
            &health,
            &AppError::Storage("late failure".to_owned()),
        );
        let report = health.report();
        assert!(report.ready);
        assert!(report.last_error.is_none());
    }

    /// Desktop maintenance loop must record `record_failure` with the
    /// underlying error string so `nagori doctor` flags a wedged retention
    /// loop. Previously the desktop dropped the result on the floor and the
    /// report always showed `consecutive_failures=0`. The helper is the
    /// single source of truth shared between the spawn body and this test
    /// — a regression that bypasses it (or swallows the failure) is caught.
    #[test]
    fn note_maintenance_outcome_records_failure_string() {
        let health = MaintenanceHealth::new();
        let err = AppError::Storage("locked".to_owned());
        let expected = err.to_string();
        let result: Result<MaintenanceReport> = Err(err);
        note_maintenance_outcome(&health, &result);
        let report = health.report();
        assert_eq!(report.consecutive_failures, 1);
        assert_eq!(report.last_error.as_deref(), Some(expected.as_str()));
    }

    /// A successful run must clear any failure recorded by an earlier
    /// iteration. The threshold-based `degraded` flag in
    /// `MaintenanceHealthReport` only resets when the counter does, so a
    /// helper that forgets to thread `Ok(_)` through `record_success`
    /// would leave the doctor surface stuck on "degraded" after recovery.
    #[test]
    fn note_maintenance_outcome_clears_state_on_success() {
        let health = MaintenanceHealth::new();
        note_maintenance_outcome(
            &health,
            &Err::<MaintenanceReport, _>(AppError::Storage("transient".to_owned())),
        );
        note_maintenance_outcome(&health, &Ok(MaintenanceReport::default()));
        let report = health.report();
        assert_eq!(report.consecutive_failures, 0);
        assert!(report.last_error.is_none());
    }

    /// Parity with the daemon's `serve.rs` path: feeding the same
    /// outcome stream into either host's `MaintenanceHealth` must produce
    /// identical `MaintenanceHealthReport`s, so `nagori doctor` reads the
    /// same fields regardless of whether the desktop or the daemon hosted
    /// the maintenance loop. The daemon's call sites are
    /// `health.record_success()` / `health.record_failure(err.to_string())`;
    /// the desktop helper above is the same two calls in the same order,
    /// and this test pins that contract so a future refactor that, e.g.,
    /// reformats the desktop's error string can't drift the two surfaces.
    #[test]
    fn maintenance_outcome_matches_daemon_recording() {
        let desktop_health = MaintenanceHealth::new();
        let daemon_health = MaintenanceHealth::new();

        let failure = AppError::Storage("disk full".to_owned());
        let failure_string = failure.to_string();
        note_maintenance_outcome(&desktop_health, &Err::<MaintenanceReport, _>(failure));
        daemon_health.record_failure(failure_string.clone());
        assert_eq!(desktop_health.report(), daemon_health.report());

        note_maintenance_outcome(&desktop_health, &Ok(MaintenanceReport::default()));
        daemon_health.record_success();
        assert_eq!(desktop_health.report(), daemon_health.report());

        // Three consecutive failures should flip both reports to
        // `degraded` simultaneously — same threshold (3) feeds both.
        for _ in 0..3 {
            let err = AppError::Storage("disk full".to_owned());
            note_maintenance_outcome(&desktop_health, &Err::<MaintenanceReport, _>(err));
            daemon_health.record_failure(failure_string.clone());
        }
        let desktop_after = desktop_health.report();
        let daemon_after = daemon_health.report();
        assert!(desktop_after.degraded);
        assert_eq!(desktop_after, daemon_after);
    }

    /// The mirror case: if the subscriber's initial `get_settings()`
    /// fails before the capture task races in, the failure must stick
    /// even if the capture task later loads settings successfully.
    /// Treating this as "intentional sticky failure" (rather than a
    /// surprise) is the deliberate trade-off documented on
    /// `StartupHealthReport` — either task aborting on settings load
    /// means the desktop is not fully running, so the gated
    /// notification stays in its failed wording until the next launch.
    #[test]
    fn subscriber_failure_sticks_even_if_capture_later_succeeds() {
        let health = StartupHealth::new();
        record_subscriber_settings_load_failure(
            &health,
            &AppError::Storage("subscriber abort".to_owned()),
        );
        let proceed = note_capture_settings_load_outcome(&health, &Ok(AppSettings::default()));
        // Helper return value still reflects the capture-side outcome
        // (so the spawn body can enter polling if its own refresh
        // landed), but the externally-visible report stays failed.
        assert!(proceed);
        let report = health.report();
        assert!(!report.ready, "subscriber failure must remain sticky");
        assert!(report.last_error.is_some());
    }

    /// `NAGORI_DB_PATH` is advertised in the startup-failure hint
    /// (`annotate_startup_error`). The resolver must actually honour it,
    /// otherwise the recovery instructions point at a no-op.
    #[test]
    fn resolve_default_db_path_honours_env_override() {
        let override_path = PathBuf::from("/custom/path/to/nagori.sqlite");
        let resolved = resolve_default_db_path(
            Some(override_path.as_os_str().to_owned()),
            Some(PathBuf::from("/should/be/ignored")),
        );
        assert_eq!(resolved, override_path);
    }

    /// Empty env value is treated as unset so a user who runs
    /// `NAGORI_DB_PATH= nagori` (intending to clear the override) doesn't
    /// end up writing the DB to a relative empty path under cwd.
    #[test]
    fn resolve_default_db_path_treats_empty_env_as_unset() {
        let resolved = resolve_default_db_path(
            Some(std::ffi::OsString::new()),
            Some(PathBuf::from("/data/local")),
        );
        assert_eq!(resolved, PathBuf::from("/data/local/nagori/nagori.sqlite"));
    }

    /// Falls back to the platform default when the env var is unset.
    #[test]
    fn resolve_default_db_path_uses_platform_default_when_env_unset() {
        let resolved = resolve_default_db_path(None, Some(PathBuf::from("/data/local")));
        assert_eq!(resolved, PathBuf::from("/data/local/nagori/nagori.sqlite"));
    }
}

/// Environment variable that overrides the default DB path resolution.
///
/// Mirrors the recovery hint baked into [`annotate_startup_error`]: when
/// the platform default directory is unwritable, the user can point
/// nagori at a path they control without rebuilding. The same variable
/// is honoured by `crates/nagori-cli/src/main.rs::default_db_path` so
/// the CLI and desktop processes target the same store when both are
/// configured against it.
pub const NAGORI_DB_PATH_ENV: &str = "NAGORI_DB_PATH";

pub fn default_db_path() -> PathBuf {
    resolve_default_db_path(std::env::var_os(NAGORI_DB_PATH_ENV), dirs::data_local_dir())
}

/// Pure path-resolution helper so unit tests don't have to mutate the
/// process environment (which is `unsafe` and races with parallel tests).
fn resolve_default_db_path(
    override_env: Option<std::ffi::OsString>,
    data_local_dir: Option<PathBuf>,
) -> PathBuf {
    if let Some(value) = override_env
        && !value.is_empty()
    {
        return PathBuf::from(value);
    }
    data_local_dir
        .unwrap_or_else(|| PathBuf::from("."))
        .join("nagori")
        .join("nagori.sqlite")
}

#[derive(Debug, Clone, Copy)]
enum StartupStage {
    Directory,
    OpenDb,
}

/// Wrap a storage failure with the file path that caused it plus a one-line
/// recovery hint. Tauri's `setup` closure has no UI yet, so the only way to
/// guide the user through DB corruption / permission errors is to put the
/// command they need to run into the error string itself.
fn annotate_startup_error(err: &AppError, db_path: &Path, stage: StartupStage) -> AppError {
    let path = db_path.display();
    let hint = match stage {
        StartupStage::Directory => format!(
            "could not prepare clipboard data directory for {path}: {err}. \
             Check that the parent directory is writable, or set NAGORI_DB_PATH \
             to a path you control"
        ),
        StartupStage::OpenDb => format!(
            "could not open clipboard database at {path}: {err}. \
             If the file is corrupted, move it aside (e.g. `mv \"{path}\" \
             \"{path}.broken\"`) and relaunch nagori — a fresh DB will be created"
        ),
    };
    AppError::Storage(hint)
}
