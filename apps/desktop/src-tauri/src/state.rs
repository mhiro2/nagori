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
    CaptureLoop, CliIpcConfig, MaintenanceHealth, MaintenanceReport, MaintenanceService,
    NagoriRuntime, ShutdownHandle, StartupHealth, WorkerRestart, spawn_cli_ipc_supervisor,
    supervise_worker,
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
    /// Single-instance lock over the data directory, held for the whole
    /// process lifetime. Two desktop instances (or a desktop instance plus a
    /// standalone daemon) owning the same store would double-capture the
    /// clipboard, race schema migrations, and let one process' clear-on-quit
    /// purge data the other still considers live. `try_new_at` acquires this
    /// before opening the store and refuses to start a second owner; `build`
    /// (used by tests with in-memory stores) leaves it `None`. The field is
    /// never read — its only job is to keep the OS lock alive until the
    /// process exits, at which point the kernel releases it.
    #[allow(dead_code)]
    instance_lock: Option<nagori_storage::ProcessLock>,
    /// Path to the clear-on-quit purge-pending marker. Set for real launches
    /// by `try_new_at`; `build`-only callers (tests / in-memory stores) own no
    /// on-disk directory and leave it `None`. When `clear_on_quit` is enabled,
    /// `perform_exit_cleanup` writes this sentinel *before* attempting the
    /// purge and removes it only once the purge completes within the shutdown
    /// budget. A launch that finds it present finishes the purge fail-closed
    /// before any window can serve history, so a timed-out / crashed shutdown
    /// purge can no longer leave behind data the user asked to clear.
    clear_on_quit_marker: Option<PathBuf>,
    /// Receiver side of the startup settings-load gate. Cloned by each gated
    /// worker (capture, CLI IPC host, settings subscriber) so they await the
    /// single coordinator load instead of each re-reading the store.
    settings_load_rx: tokio::sync::watch::Receiver<SettingsLoadGate>,
    /// Sender side, taken once by the coordinator in `spawn_background_tasks`.
    /// Held in a slot rather than moved into `build`'s return so the coordinator
    /// can publish the load outcome after the state is managed by Tauri.
    settings_load_tx: Mutex<Option<tokio::sync::watch::Sender<SettingsLoadGate>>>,
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

/// Outcome of the one-shot startup settings load, broadcast to the workers
/// that must not run until it lands. Mirrors the daemon's `run_daemon`, which
/// loads settings once up front and only then spawns its workers — here a
/// single coordinator does the load and publishes the result through this
/// gate so the capture loop, the CLI IPC host, and the settings subscriber
/// each start from the same snapshot instead of re-reading the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsLoadGate {
    /// The coordinator has not finished the initial load yet.
    Pending,
    /// Settings loaded and were published to the runtime's watch channel.
    Loaded,
    /// The initial load failed; gated workers must not start (fail-closed).
    Failed,
}

/// Block until the startup settings load resolves. Returns `true` once it
/// loaded, `false` if it failed — or if the coordinator died before resolving
/// (the watch sender dropped while still `Pending`), which is treated as a
/// failure so a crashed coordinator fails closed rather than wedging the
/// gated workers forever. Resolves immediately when the gate is already set.
pub(crate) async fn settings_loaded_ok(
    gate: &mut tokio::sync::watch::Receiver<SettingsLoadGate>,
) -> bool {
    match gate
        .wait_for(|state| *state != SettingsLoadGate::Pending)
        .await
    {
        Ok(state) => *state == SettingsLoadGate::Loaded,
        Err(_) => false,
    }
}

/// [`settings_loaded_ok`], but also gives up (returns `false` without
/// proceeding) the moment shutdown is signalled. Production always starts the
/// coordinator before any gated worker, so the gate resolves on its own; this
/// is a backstop so a gated worker can never outlive teardown — and can never
/// hang at all should the gate, by misconfiguration, never be published.
pub(crate) async fn settings_loaded_or_shutdown(
    gate: &mut tokio::sync::watch::Receiver<SettingsLoadGate>,
    shutdown: &mut ShutdownHandle,
) -> bool {
    tokio::select! {
        ok = settings_loaded_ok(gate) => ok,
        () = shutdown.cancelled() => false,
    }
}

struct BackgroundTasks {
    settings_load: tauri::async_runtime::JoinHandle<()>,
    capture: tauri::async_runtime::JoinHandle<()>,
    maintenance: tauri::async_runtime::JoinHandle<()>,
    semantic: tauri::async_runtime::JoinHandle<()>,
    ngram_rebuild: tauri::async_runtime::JoinHandle<()>,
    ai_watchdog: tauri::async_runtime::JoinHandle<()>,
    cli_ipc: tauri::async_runtime::JoinHandle<()>,
    ipc_mutations: tauri::async_runtime::JoinHandle<()>,
}

/// Grace each supervised worker gets to drain in-flight work when shutdown is
/// requested. The worker exits between ticks (so a partway-through DB write
/// commits instead of being abandoned); past this it is force-aborted.
const WORKER_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Extra drain budget for the CLI IPC host on top of the caller's worker
/// grace. The IPC supervisor's shutdown branch (`stop_ipc_server`) waits
/// `shutdown_grace + 1s` for in-flight handlers, then — when one is wedged
/// — aborts it and waits up to another 2s for the post-abort join, before
/// it finishes removing the socket / token files. With the default 5s
/// grace that is 8s worst-case, so the slack must put the outer budget
/// beyond it (5s + 4s = 9s, leaving ~1s for the file cleanup itself);
/// a shorter budget could abort the supervisor between the drain and the
/// cleanup and leave a stale socket behind. Matches the accounting in the
/// daemon's `drain_workers` (`grace + POST_ABORT_JOIN_TIMEOUT + 1s`).
const CLI_IPC_DRAIN_SLACK: Duration = Duration::from_secs(4);

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
                .map_err(|err| annotate_startup_error(err, db_path, StartupStage::Directory))?;
        }
        // Take the single-instance lock *before* opening the store, so a
        // second launch never runs migrations or starts a capture loop
        // against a DB the first instance already owns. The lock lives in the
        // DB's parent directory, which the daemon also locks on the default
        // layout, so launching the app while a standalone daemon owns the same
        // store is refused too.
        let instance_lock = acquire_instance_lock(lock_dir_for(db_path))?;
        let store = SqliteStore::open(db_path)
            .map_err(|err| annotate_startup_error(err, db_path, StartupStage::OpenDb))?;
        let mut state = Self::build(store)?;
        state.instance_lock = Some(instance_lock);
        state.clear_on_quit_marker = Some(clear_on_quit_marker_path(db_path));
        // Fail-closed: complete any clear-on-quit purge the previous session
        // could not finish before the state is handed to Tauri. A failure here
        // surfaces the startup fallback window (and leaves the marker) rather
        // than booting into a session that still shows the cleared history.
        state.finish_pending_clear_on_quit()?;
        Ok(state)
    }

    /// Spawns the in-process capture, maintenance, semantic-index, ngram-rebuild
    /// and AI-watchdog workers. Call once after `manage(state)` so a Tokio
    /// runtime is available.
    ///
    /// Each worker runs under [`supervise_worker`] — the same respawn-and-drain
    /// policy the CLI daemon uses — so a panic or unexpected early return
    /// restarts the worker (with backoff) instead of silently leaving the app
    /// running with a dead loop and a stale/falsely-healthy snapshot. The
    /// ngram backfill is one-shot, so its supervisor only respawns on a panic.
    /// The supervisor task handle is what we store; on shutdown each supervisor
    /// drains its live worker within [`WORKER_DRAIN_GRACE`] before returning.
    pub fn spawn_background_tasks(&self, app: tauri::AppHandle) {
        let mut tasks_slot = self.background_tasks_slot();
        if tasks_slot.is_some() {
            tracing::warn!("background_tasks_already_started");
            return;
        }

        *tasks_slot = Some(BackgroundTasks {
            // Spawned first so the single store read starts immediately; the
            // gated workers below await its outcome via `settings_load_gate`.
            settings_load: self.spawn_settings_load_coordinator(),
            ipc_mutations: spawn_ipc_mutation_forwarder(
                &self.runtime,
                self.runtime.shutdown_handle(),
                app.clone(),
            ),
            capture: spawn_capture_supervisor(
                self.runtime.clone(),
                self.window.clone(),
                self.capture_reader.clone(),
                self.settings_load_gate(),
                self.runtime.shutdown_handle(),
                app,
            ),
            maintenance: spawn_maintenance_supervisor(
                self.runtime.clone(),
                self.runtime.shutdown_handle(),
            ),
            semantic: spawn_semantic_supervisor(
                self.runtime.clone(),
                self.runtime.shutdown_handle(),
            ),
            ngram_rebuild: spawn_ngram_rebuild_supervisor(
                self.runtime.clone(),
                self.runtime.shutdown_handle(),
            ),
            ai_watchdog: spawn_ai_watchdog_supervisor(
                self.runtime.clone(),
                self.runtime.shutdown_handle(),
            ),
            cli_ipc: self.spawn_cli_ipc_host(CliIpcConfig::default()),
        });
    }

    /// Run the one-shot startup settings load and publish its outcome to the
    /// gate. A single load here — mirroring the daemon's `run_daemon`, which
    /// loads settings once before spawning its workers — replaces the
    /// per-worker `refresh_settings_from_store` calls the capture loop, the
    /// CLI IPC host, and the settings subscriber each used to make, so they
    /// all observe one consistent snapshot rather than racing concurrent
    /// reads. It records the startup health (the gated "nagori is running"
    /// notification reads it) and then sends `Loaded` / `Failed` through the
    /// watch. If this task dies before sending, the dropped sender resolves
    /// every waiter to failed (fail-closed).
    fn spawn_settings_load_coordinator(&self) -> tauri::async_runtime::JoinHandle<()> {
        let tx = self
            .settings_load_tx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let runtime = self.runtime.clone();
        tauri::async_runtime::spawn(async move {
            // `spawn_background_tasks` guards against a second start, so the
            // slot is present on the first (and only) call; bail quietly if a
            // future caller ever spawns the coordinator twice.
            let Some(tx) = tx else {
                tracing::warn!("settings_load_coordinator_already_started");
                return;
            };
            let outcome = runtime.refresh_settings_from_store().await;
            let gate = if note_settings_load_outcome(&runtime.startup_health(), &outcome) {
                SettingsLoadGate::Loaded
            } else {
                SettingsLoadGate::Failed
            };
            // Receivers retain the last value, so dropping `tx` after this
            // send still lets a late waiter observe the terminal state.
            let _ = tx.send(gate);
        })
    }

    /// Host the CLI IPC endpoint inside the desktop process so `nagori`
    /// write commands reach this runtime (and its search-cache
    /// invalidation) while the GUI is running, exactly as they would a
    /// headless `nagori daemon run`.
    ///
    /// Two gates precede the supervisor:
    ///
    /// * The single-instance lock must be held. The bind path reclaims a
    ///   leftover socket on the grounds that the lock holder is the only
    ///   live owner of the store — a `build()`-only state (tests,
    ///   in-memory) holds no lock, so it must not bind a real endpoint.
    /// * The startup settings load must have succeeded (fail-closed).
    ///   `cli_ipc_enabled` is read from the runtime's settings snapshot, so
    ///   serving before the store snapshot lands would honor the compiled-in
    ///   default — and briefly expose the socket — even when the user
    ///   disabled CLI IPC. The host awaits the shared coordinator gate rather
    ///   than reading the store itself.
    fn spawn_cli_ipc_host(&self, config: CliIpcConfig) -> tauri::async_runtime::JoinHandle<()> {
        if self.instance_lock.is_none() {
            tracing::info!("cli_ipc_skipped_without_instance_lock");
            return tauri::async_runtime::spawn(async {});
        }
        let runtime = self.runtime.clone();
        let mut gate = self.settings_load_gate();
        tauri::async_runtime::spawn(async move {
            let mut shutdown = runtime.shutdown_handle();
            if !settings_loaded_or_shutdown(&mut gate, &mut shutdown).await {
                tracing::warn!("cli_ipc_settings_load_failed_not_serving");
                return;
            }
            let supervisor = spawn_cli_ipc_supervisor(runtime, config, shutdown);
            if let Err(err) = supervisor.await {
                tracing::warn!(error = %err, "cli_ipc_supervisor_join_failed");
            }
        })
    }

    /// Cancel, drain, and abort the in-process supervised workers. Safe to call
    /// more than once; only the first call owns the task handles.
    ///
    /// The stored handles are now the *supervisor* tasks: cancelling the shared
    /// shutdown signal makes each supervisor drain its live worker within
    /// [`WORKER_DRAIN_GRACE`] (force-aborting a wedged one) before returning, so
    /// `grace` here must exceed `WORKER_DRAIN_GRACE` plus the supervisor's own
    /// post-abort join window. The caller passes a budget sized for that.
    pub async fn shutdown_background_tasks(&self, grace: Duration) {
        self.runtime.shutdown_handle().cancel();
        let Some(tasks) = self.background_tasks_slot().take() else {
            return;
        };
        tokio::join!(
            drain_background_task("settings_load", tasks.settings_load, grace),
            drain_background_task("capture", tasks.capture, grace),
            drain_background_task("maintenance", tasks.maintenance, grace),
            drain_background_task("semantic", tasks.semantic, grace),
            drain_background_task("ngram_rebuild", tasks.ngram_rebuild, grace),
            drain_background_task("ai_watchdog", tasks.ai_watchdog, grace),
            // The IPC supervisor drains in-flight handlers for its own
            // `shutdown_grace + 1s` before it can remove the socket / token
            // files, so it gets extra slack on top of the shared budget —
            // see CLI_IPC_DRAIN_SLACK.
            drain_background_task("cli_ipc", tasks.cli_ipc, grace + CLI_IPC_DRAIN_SLACK),
            drain_background_task("ipc_mutations", tasks.ipc_mutations, grace),
        );
    }

    fn background_tasks_slot(&self) -> std::sync::MutexGuard<'_, Option<BackgroundTasks>> {
        self.background_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn build(store: SqliteStore) -> Result<Self> {
        let parts = build_native_runtime(store, NativeRuntimeOptions::default())?;
        let (settings_load_tx, settings_load_rx) =
            tokio::sync::watch::channel(SettingsLoadGate::Pending);
        Ok(Self {
            runtime: parts.runtime,
            window: parts.window,
            preview: parts.preview,
            capture_reader: parts.clipboard_reader,
            background_tasks: Mutex::new(None),
            previous_frontmost: Arc::new(Mutex::new(None)),
            last_pasted_id: Mutex::new(None),
            last_hotkey_failure: Mutex::new(HotkeyFailureCache::default()),
            // Set by `try_new_at` for real launches; `build`-only callers
            // (tests, in-memory stores) own no on-disk directory to lock.
            instance_lock: None,
            clear_on_quit_marker: None,
            settings_load_rx,
            settings_load_tx: Mutex::new(Some(settings_load_tx)),
        })
    }

    /// Clone the startup settings-load gate receiver so a worker spawned
    /// outside `spawn_background_tasks` (the settings subscriber, in `lib.rs`)
    /// can await the single coordinator load via [`settings_loaded_ok`].
    pub(crate) fn settings_load_gate(&self) -> tokio::sync::watch::Receiver<SettingsLoadGate> {
        self.settings_load_rx.clone()
    }

    /// Write the clear-on-quit purge-pending marker. Called by the shutdown
    /// path *before* it attempts the purge so a timeout, crash, or kill
    /// mid-purge is finished on the next launch instead of leaving the history
    /// the user asked to clear. No-op (`Ok`) for `build`-only callers (no
    /// marker path). The `io::Result` is surfaced rather than swallowed: a
    /// write failure means a subsequent purge timeout could not be resumed, so
    /// the caller logs it instead of silently losing the guarantee.
    pub fn mark_clear_on_quit_pending(&self) -> std::io::Result<()> {
        let Some(path) = self.clear_on_quit_marker.as_ref() else {
            return Ok(());
        };
        std::fs::write(path, b"")
    }

    /// Remove the purge-pending marker after a purge has actually completed.
    /// A missing file is success (the marker was never written or already
    /// gone). A real removal failure is returned, not swallowed: a marker left
    /// behind after a *successful* purge would make the next launch purge again
    /// — including any history captured in between — so the caller must treat
    /// it as a hard error rather than logging and moving on.
    pub fn clear_clear_on_quit_pending(&self) -> std::io::Result<()> {
        let Some(path) = self.clear_on_quit_marker.as_ref() else {
            return Ok(());
        };
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// Whether a clear-on-quit purge from a previous session is still pending,
    /// i.e. the marker survived because the shutdown purge did not complete.
    /// An I/O error probing the marker is treated as **present** so an
    /// unreadable marker fails closed into a purge attempt rather than silently
    /// skipping the user's clear-on-quit intent.
    #[must_use]
    pub fn clear_on_quit_marker_present(&self) -> bool {
        let Some(path) = self.clear_on_quit_marker.as_ref() else {
            return false;
        };
        match path.try_exists() {
            Ok(present) => present,
            Err(err) => {
                tracing::warn!(error = %err, "clear_on_quit_marker_probe_failed");
                true
            }
        }
    }

    /// Finish a clear-on-quit purge a previous session could not complete
    /// within its shutdown budget. Runs during `try_new_at` — before the state
    /// is handed to Tauri and before any window can serve a row — so the purge
    /// is fail-closed: if the marker is present the purge must succeed (and its
    /// marker must be removed) before the app starts normally. A failure is
    /// returned as a startup error, which surfaces the fallback window and
    /// leaves the marker so the next launch retries, rather than booting into a
    /// session that still shows — or later re-purges — the history the user
    /// asked to clear.
    fn finish_pending_clear_on_quit(&self) -> Result<()> {
        if !self.clear_on_quit_marker_present() {
            return Ok(());
        }
        tracing::warn!("clear_on_quit_resuming_pending_purge");
        tauri::async_runtime::block_on(self.runtime.clear_non_pinned()).map_err(|err| {
            AppError::storage(format!(
                "could not finish the clear-on-quit purge left pending by the previous session: \
                 {err}. The clipboard history you asked to clear on quit is still present; relaunch \
                 to retry, or move the database aside to start fresh."
            ))
        })?;
        self.clear_clear_on_quit_pending().map_err(|err| {
            AppError::storage(format!(
                "finished the clear-on-quit purge but could not remove its marker: {err}. Remove \
                 the clear-on-quit.pending file in the database directory so the next launch does \
                 not purge again."
            ))
        })?;
        Ok(())
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

/// Supervise the in-process clipboard capture loop. `reader` / `window` are
/// shared (`Arc`) so each respawn after a panic rebuilds a fresh
/// [`CaptureLoop`] over the same adapter. The loop waits for the one-shot
/// startup settings load (the coordinator's `settings_load_gate`) *before* it
/// is entered: if that load failed the worker never enters the loop (so a
/// persistent settings failure leaves capture down rather than respawn-spinning),
/// mirroring the daemon's `run_daemon`.
fn spawn_capture_supervisor(
    runtime: NagoriRuntime,
    window: Arc<dyn WindowBehavior>,
    reader: Arc<dyn ClipboardReader>,
    mut settings_gate: tokio::sync::watch::Receiver<SettingsLoadGate>,
    shutdown: ShutdownHandle,
    app: tauri::AppHandle,
) -> tauri::async_runtime::JoinHandle<()> {
    let mut gate_shutdown = shutdown.clone();
    tauri::async_runtime::spawn(async move {
        if !settings_loaded_or_shutdown(&mut settings_gate, &mut gate_shutdown).await {
            return;
        }
        supervise_worker(
            "capture",
            WorkerRestart::OnExit,
            shutdown,
            WORKER_DRAIN_GRACE,
            move |mut worker_shutdown| {
                let runtime = runtime.clone();
                let reader = reader.clone();
                let window = window.clone();
                let app = app.clone();
                let store = runtime.store().clone();
                let settings = runtime.current_settings();
                let search_cache = runtime.search_cache_handle();
                let capture_health = runtime.capture_health();
                let settings_rx = runtime.settings_subscribe();
                tokio::spawn(async move {
                    let app_for_capture_event = app.clone();
                    let runtime_for_notify = runtime.clone();
                    let capture_notifier = Arc::new(move |entry_id: EntryId| {
                        use tauri::Emitter;

                        let _ = app_for_capture_event.emit(
                            crate::CLIPBOARD_CHANGED_EVENT,
                            serde_json::json!({ "entryId": entry_id.to_string() }),
                        );
                        // Nudge the semantic indexer so the fresh clip is
                        // embedded promptly (no-op when the index is disabled
                        // / unsupported).
                        runtime_for_notify.notify_semantic_capture();
                    });
                    let mut capture =
                        CaptureLoop::new(reader, store.clone(), store.clone(), settings)
                            .with_window(window)
                            .with_search_cache(search_cache)
                            .with_capture_health(capture_health)
                            .with_capture_notifier(capture_notifier);
                    let shutdown_signal = async move { worker_shutdown.cancelled().await };
                    if let Err(err) = capture
                        .run_polling_with_settings(
                            Duration::from_millis(500),
                            settings_rx,
                            shutdown_signal,
                        )
                        .await
                    {
                        tracing::warn!(error = %err, "capture_loop_terminated");
                    }
                })
            },
        )
        .await;
    })
}

/// Forward corpus mutations made by external IPC clients (`nagori add` /
/// `delete` / `pin` / `clear`, plus the ranking-relevant use-count bumps
/// of `copy` / `paste`) to the palette's refresh event.
///
/// The capture loop's notifier covers clipboard captures and the palette
/// refreshes itself after its own commands, but an IPC write has no other
/// path to the frontend — without this, a CLI `nagori add` only shows up
/// whenever the next capture happens to fire. Reuses
/// `CLIPBOARD_CHANGED_EVENT` (whose payload the palette ignores) so the
/// frontend contract stays a single "re-run your query" signal.
///
/// A plain forwarding loop, not a `supervise_worker`: it holds no state
/// and cannot fail other than by the channel closing, which only happens
/// at runtime teardown.
fn spawn_ipc_mutation_forwarder(
    runtime: &NagoriRuntime,
    mut shutdown: ShutdownHandle,
    app: tauri::AppHandle,
) -> tauri::async_runtime::JoinHandle<()> {
    // Subscribe synchronously, before the task is scheduled: the CLI IPC
    // host is spawned right after this call, and a mutation that lands
    // between the two must wake the first `changed()` below rather than
    // race the task's startup. The baseline is deliberately NOT marked
    // seen — if a mutation somehow predates the subscription, one
    // catch-up refresh fires, which is harmless; swallowing it is not.
    let mut mutations = runtime.external_mutations_subscribe();
    tauri::async_runtime::spawn(async move {
        use tauri::Emitter;
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                changed = mutations.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    let _ = app.emit(crate::CLIPBOARD_CHANGED_EVENT, serde_json::json!({}));
                }
            }
        }
    })
}

/// Supervise the periodic maintenance loop (retention sweep).
fn spawn_maintenance_supervisor(
    runtime: NagoriRuntime,
    shutdown: ShutdownHandle,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        supervise_worker(
            "maintenance",
            WorkerRestart::OnExit,
            shutdown,
            WORKER_DRAIN_GRACE,
            move |mut worker_shutdown| {
                let runtime = runtime.clone();
                let store = runtime.store().clone();
                let health = runtime.maintenance_health();
                let search_cache = runtime.search_cache_handle();
                let mut settings_rx = runtime.settings_subscribe();
                tokio::spawn(async move {
                    let maintenance =
                        MaintenanceService::new(store).with_search_cache(search_cache);
                    loop {
                        let settings = settings_rx.borrow().clone();
                        let outcome = maintenance.run(&settings).await;
                        note_maintenance_outcome(&health, &outcome);
                        tokio::select! {
                            () = worker_shutdown.cancelled() => return,
                            _ = settings_rx.changed() => {},
                            () = tokio::time::sleep(Duration::from_mins(30)) => {},
                        }
                    }
                })
            },
        )
        .await;
    })
}

/// Supervise the background semantic-index worker.
fn spawn_semantic_supervisor(
    runtime: NagoriRuntime,
    shutdown: ShutdownHandle,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        supervise_worker(
            "semantic",
            WorkerRestart::OnExit,
            shutdown,
            WORKER_DRAIN_GRACE,
            move |worker_shutdown| {
                let runtime = runtime.clone();
                tokio::spawn(async move { runtime.run_semantic_indexer(worker_shutdown).await })
            },
        )
        .await;
    })
}

/// Supervise the one-shot ngram-rebuild backfill of ngrams left stale by a
/// generator upgrade (kana folding / Han 1-grams). The desktop app drives
/// `NagoriRuntime` directly without the CLI daemon's serve loop, so it must
/// spawn this worker itself — otherwise a desktop-only history never gets its
/// old rows rebuilt and CJK search improvements don't apply to them. A clean
/// completion is terminal (the backlog drained); only a panic respawns it.
fn spawn_ngram_rebuild_supervisor(
    runtime: NagoriRuntime,
    shutdown: ShutdownHandle,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        supervise_worker(
            "ngram_rebuild",
            WorkerRestart::OnPanic,
            shutdown,
            WORKER_DRAIN_GRACE,
            move |worker_shutdown| {
                let runtime = runtime.clone();
                tokio::spawn(async move { runtime.run_ngram_rebuild(worker_shutdown).await })
            },
        )
        .await;
    })
}

/// Supervise the dedicated AI stale-request watchdog. The desktop drives AI
/// actions directly (no daemon maintenance loop), so without this a leaked or
/// wedged AI stream's concurrency permit would never be reclaimed.
fn spawn_ai_watchdog_supervisor(
    runtime: NagoriRuntime,
    shutdown: ShutdownHandle,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        supervise_worker(
            "ai_watchdog",
            WorkerRestart::OnExit,
            shutdown,
            WORKER_DRAIN_GRACE,
            move |worker_shutdown| {
                let runtime = runtime.clone();
                tokio::spawn(async move {
                    runtime.run_ai_request_watchdog(worker_shutdown).await;
                })
            },
        )
        .await;
    })
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

/// Funnels the startup settings-load outcome into the shared `StartupHealth`
/// signal and decides whether the gated workers should proceed. Called once
/// by the settings-load coordinator (the gated capture loop, CLI IPC host,
/// and settings subscriber then key off its broadcast gate). Extracted so the
/// wiring between "settings load failed" and "`StartupHealth` records failed"
/// can be pinned by a unit test rather than living only inside
/// `tauri::async_runtime::spawn`, where an inline version silently dropping
/// failures left users with a "Clipboard history is ready" notification while
/// capture never started.
pub(crate) fn note_settings_load_outcome(
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
            tracing::error!(error = %err, "settings_load_failed_aborting_workers");
            false
        }
    }
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

    struct StubWindowBehavior;

    #[async_trait::async_trait]
    impl WindowBehavior for StubWindowBehavior {
        async fn frontmost_app(&self) -> Result<Option<nagori_platform::FrontmostApp>> {
            Ok(None)
        }

        async fn show_palette(&self) -> Result<()> {
            Ok(())
        }

        async fn hide_palette(&self) -> Result<()> {
            Ok(())
        }
    }

    /// Build an `AppState` over in-memory adapters. These tests exercise
    /// desktop-side wiring (the settings gate, the CLI IPC host, shutdown
    /// draining), not platform adapters — and `AppState::build` initialises
    /// the host's real clipboard, which needs a live session (a Wayland
    /// compositor on Linux) that headless CI runners don't provide.
    fn build_test_state() -> AppState {
        use nagori_platform::{MemoryClipboard, UnsupportedPreviewController};

        let clipboard = Arc::new(MemoryClipboard::new());
        let runtime = NagoriRuntime::builder(SqliteStore::open_memory().expect("memory store"))
            .clipboard(clipboard.clone())
            .build_for_test();
        let (settings_load_tx, settings_load_rx) =
            tokio::sync::watch::channel(SettingsLoadGate::Pending);
        AppState {
            runtime,
            window: Arc::new(StubWindowBehavior),
            preview: Arc::new(UnsupportedPreviewController),
            capture_reader: clipboard,
            background_tasks: Mutex::new(None),
            previous_frontmost: Arc::new(Mutex::new(None)),
            last_pasted_id: Mutex::new(None),
            last_hotkey_failure: Mutex::new(HotkeyFailureCache::default()),
            instance_lock: None,
            clear_on_quit_marker: None,
            settings_load_rx,
            settings_load_tx: Mutex::new(Some(settings_load_tx)),
        }
    }

    #[cfg(unix)]
    mod cli_ipc {
        use super::*;

        fn test_ipc_config(dir: &std::path::Path) -> CliIpcConfig {
            CliIpcConfig {
                socket_path: dir.join("nagori.sock"),
                token_path: dir.join("nagori.token"),
                shutdown_grace: Duration::from_millis(50),
                ..CliIpcConfig::default()
            }
        }

        async fn wait_until(
            timeout: Duration,
            mut predicate: impl FnMut() -> bool,
        ) -> std::result::Result<(), ()> {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if predicate() {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(())
        }

        /// A `build()`-only state holds no single-instance lock, so the
        /// IPC host must refuse to bind: the stale-socket reclaim inside
        /// the bind path is only sound while this process owns the
        /// data-directory lock.
        #[tokio::test]
        async fn host_skips_without_instance_lock() {
            let temp = tempfile::tempdir().expect("temp dir");
            let state = build_test_state();
            let config = test_ipc_config(temp.path());
            let handle = state.spawn_cli_ipc_host(config.clone());
            tokio::time::timeout(Duration::from_secs(1), handle)
                .await
                .expect("skip task should finish promptly")
                .expect("skip task should not panic");
            assert!(
                !config.socket_path.exists(),
                "lockless state must not bind the socket",
            );
            assert!(
                !config.token_path.exists(),
                "lockless state must not write a token file",
            );
        }

        /// End-to-end over the desktop host: the endpoint answers Health
        /// while the app runs, and shutdown removes the socket and token
        /// within the drain budget.
        #[tokio::test]
        async fn host_serves_health_and_cleans_up_on_shutdown() {
            let temp = tempfile::tempdir().expect("temp dir");
            let mut state = build_test_state();
            state.instance_lock = Some(
                nagori_storage::ProcessLock::try_acquire(temp.path())
                    .expect("lock io")
                    .expect("lock should be free"),
            );
            let config = test_ipc_config(temp.path());
            // The host gates on the startup settings load, so drive the
            // coordinator the same way `spawn_background_tasks` would —
            // otherwise the gate stays Pending and the host never binds.
            let _coordinator = state.spawn_settings_load_coordinator();
            let handle = state.spawn_cli_ipc_host(config.clone());

            // Wait for the token too, not just the socket: the bind creates the
            // socket inode a beat before the token file is written, so polling
            // on the socket alone can race the token read below under load.
            wait_until(Duration::from_secs(3), || {
                config.socket_path.exists() && config.token_path.exists()
            })
            .await
            .expect("socket and token should appear once the host is up");
            let token = nagori_ipc::read_token_file(&config.token_path).expect("token file");
            let client = nagori_ipc::IpcClient::new(
                config.socket_path.to_string_lossy().into_owned(),
                token,
            )
            .with_connect_timeout(Duration::from_millis(100))
            .with_request_timeout(Duration::from_secs(1));
            let health = client
                .send(nagori_ipc::IpcRequest::Health)
                .await
                .expect("health over the desktop-hosted endpoint");
            assert!(matches!(health, nagori_ipc::IpcResponse::Health(_)));

            state.runtime.shutdown_handle().cancel();
            tokio::time::timeout(Duration::from_secs(3), handle)
                .await
                .expect("host should stop after shutdown")
                .expect("host should not panic");
            assert!(
                !config.socket_path.exists(),
                "socket should be removed on shutdown",
            );
            assert!(
                !config.token_path.exists(),
                "token file should be removed on shutdown",
            );
        }

        /// Fail-closed: when the startup settings load failed (the gate
        /// resolved to `Failed`), the host must never bind — serving on the
        /// compiled-in default could expose the socket even though the user
        /// disabled CLI IPC.
        #[tokio::test]
        async fn host_does_not_serve_when_settings_load_failed() {
            let temp = tempfile::tempdir().expect("temp dir");
            let mut state = build_test_state();
            state.instance_lock = Some(
                nagori_storage::ProcessLock::try_acquire(temp.path())
                    .expect("lock io")
                    .expect("lock should be free"),
            );
            let config = test_ipc_config(temp.path());
            // Stand in for the coordinator reporting a failed load.
            state
                .settings_load_tx
                .lock()
                .expect("tx slot")
                .take()
                .expect("tx present")
                .send(SettingsLoadGate::Failed)
                .expect("publish failed gate");

            let handle = state.spawn_cli_ipc_host(config.clone());
            tokio::time::timeout(Duration::from_secs(1), handle)
                .await
                .expect("host should bail promptly on a failed gate")
                .expect("host should not panic");
            assert!(
                !config.socket_path.exists(),
                "a failed settings load must not bind the socket",
            );
            assert!(
                !config.token_path.exists(),
                "a failed settings load must not write a token file",
            );
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

    /// Settings-load abort path: when the coordinator's load returns an
    /// error, `StartupHealth` must flip to `failed` with the error string
    /// preserved verbatim. This pins the wiring extracted out of
    /// `spawn_settings_load_coordinator` so a future inline refactor that
    /// drops the recording is caught even without running the full spawn.
    #[test]
    fn note_settings_load_outcome_records_failure() {
        let health = StartupHealth::new();
        let err = AppError::storage("disk full".to_owned());
        let expected = err.to_string();
        let result: Result<AppSettings> = Err(err);
        let proceed = note_settings_load_outcome(&health, &result);
        assert!(
            !proceed,
            "gated workers must abort when settings load fails"
        );
        let report = health.report();
        assert!(!report.ready);
        assert_eq!(report.last_error.as_deref(), Some(expected.as_str()));
    }

    /// Settings-load success path: a settled load must flip ready, with no
    /// error recorded. Combined with the failure test, this fixes the helper
    /// as the single source of truth for "did startup reach a serving
    /// state?" — the gated "Nagori is running" notification reads it.
    #[test]
    fn note_settings_load_outcome_records_ready_on_success() {
        let health = StartupHealth::new();
        let result: Result<AppSettings> = Ok(AppSettings::default());
        let proceed = note_settings_load_outcome(&health, &result);
        assert!(proceed, "gated workers must continue when settings load");
        let report = health.report();
        assert!(report.ready);
        assert!(report.last_error.is_none());
    }

    /// The startup gate must resolve a gated worker to the coordinator's
    /// terminal state — and, crucially, never wedge it: a coordinator that
    /// dies before publishing (its sender dropped while still `Pending`)
    /// must resolve to a fail-closed `false` rather than blocking forever.
    #[tokio::test]
    async fn settings_loaded_ok_resolves_to_the_gate_state() {
        use tokio::sync::watch;

        let (tx, mut rx) = watch::channel(SettingsLoadGate::Pending);
        tx.send(SettingsLoadGate::Loaded).expect("send loaded");
        assert!(
            settings_loaded_ok(&mut rx).await,
            "Loaded gate must proceed"
        );

        let (tx, mut rx) = watch::channel(SettingsLoadGate::Pending);
        tx.send(SettingsLoadGate::Failed).expect("send failed");
        assert!(
            !settings_loaded_ok(&mut rx).await,
            "Failed gate must not proceed"
        );

        // Sender dropped while still Pending → fail-closed, no hang.
        let (tx, mut rx) = watch::channel(SettingsLoadGate::Pending);
        drop(tx);
        assert!(
            !settings_loaded_ok(&mut rx).await,
            "a coordinator that dies before resolving must fail closed",
        );

        // Late waiter: the terminal value is retained after the sender drops.
        let (tx, mut rx) = watch::channel(SettingsLoadGate::Pending);
        tx.send(SettingsLoadGate::Loaded).expect("send loaded");
        drop(tx);
        assert!(
            settings_loaded_ok(&mut rx).await,
            "a retained Loaded value must still resolve after the sender drops",
        );
    }

    /// Backstop against a wedge: a gated worker awaiting a gate that never
    /// resolves (its sender held, e.g. the coordinator was never started)
    /// must still give up the moment shutdown is signalled rather than wait
    /// forever. The `tx` is deliberately kept alive so the gate stays Pending.
    #[tokio::test]
    async fn settings_loaded_or_shutdown_gives_up_on_shutdown() {
        let state = build_test_state();
        let mut shutdown = state.runtime.shutdown_handle();
        let (_tx, mut rx) = tokio::sync::watch::channel(SettingsLoadGate::Pending);

        shutdown.cancel();
        assert!(
            !settings_loaded_or_shutdown(&mut rx, &mut shutdown).await,
            "a pending gate must not block past shutdown",
        );
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
        let err = AppError::storage("locked".to_owned());
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
            &Err::<MaintenanceReport, _>(AppError::storage("transient".to_owned())),
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

        let failure = AppError::storage("disk full".to_owned());
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
            let err = AppError::storage("disk full".to_owned());
            note_maintenance_outcome(&desktop_health, &Err::<MaintenanceReport, _>(err));
            daemon_health.record_failure(failure_string.clone());
        }
        let desktop_after = desktop_health.report();
        let daemon_after = daemon_health.report();
        assert!(desktop_after.degraded);
        assert_eq!(desktop_after, daemon_after);
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

/// Directory whose `nagori.lock` the single-instance lock lives in: the DB's
/// parent, falling back to the current directory for a bare relative DB
/// filename. Kept in lockstep with the daemon's choice
/// (`nagori_daemon::serve::acquire_daemon_lock`, keyed on the socket parent)
/// so that on the default layout — DB and socket share one directory — the
/// app and a standalone daemon contend for the same lock file.
fn lock_dir_for(db_path: &Path) -> &Path {
    match db_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// Path to the clear-on-quit purge-pending marker: a sentinel file beside the
/// DB. Kept on the filesystem rather than in the DB so it can be written /
/// checked even when the DB itself is the contended resource that made the
/// shutdown purge time out, and so a hard kill that never ran any DB write
/// still leaves a trace the next launch can act on.
///
/// Keyed to the DB *file*, not just its directory: a marker named after the
/// directory alone would let a different `NAGORI_DB_PATH` pointed at another DB
/// in the same directory inherit a stale marker and purge the wrong database's
/// history at startup. Appending the suffix to the full path yields e.g.
/// `…/nagori.sqlite.clear-on-quit.pending`.
fn clear_on_quit_marker_path(db_path: &Path) -> PathBuf {
    let mut marker = db_path.as_os_str().to_owned();
    marker.push(".clear-on-quit.pending");
    PathBuf::from(marker)
}

/// Acquire the single-instance lock over `lock_dir`, mapping contention to a
/// self-explanatory startup error. The `setup` closure surfaces this message
/// in the fallback window (the only UI a duplicate launch reaches), so it must
/// tell the user where the running instance is rather than failing silently.
fn acquire_instance_lock(lock_dir: &Path) -> Result<nagori_storage::ProcessLock> {
    match nagori_storage::ProcessLock::try_acquire(lock_dir)? {
        Some(lock) => Ok(lock),
        None => Err(AppError::Platform(format!(
            "another nagori process is already using the clipboard history in {}. \
             Only one instance can own it at a time — look for the running nagori in \
             your menu bar / system tray, or use the global shortcut to open the \
             palette.",
            lock_dir.display()
        ))),
    }
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
fn annotate_startup_error(err: AppError, db_path: &Path, stage: StartupStage) -> AppError {
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
    // The hint repeats the original error's Display form for the dialog, but
    // keep the typed cause (`rusqlite::Error`, `io::Error`, …) in the source
    // chain instead of flattening it into the string.
    AppError::storage_with(hint, err)
}
