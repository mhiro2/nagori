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
use nagori_daemon::{CaptureLoop, MaintenanceService, NagoriRuntime, StartupHealth};
use nagori_platform_native::{NativeRuntimeOptions, build_native_runtime};
use nagori_storage::SqliteStore;

use nagori_platform::{ClipboardReader, RestoreTarget, WindowBehavior};
#[cfg(target_os = "macos")]
use nagori_platform_macos::MacosWindowBehavior;
#[cfg(target_os = "windows")]
use nagori_platform_windows::WindowsWindowBehavior;

pub struct AppState {
    pub runtime: NagoriRuntime,
    pub window: Arc<dyn WindowBehavior>,
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
}

struct BackgroundTasks {
    capture: tauri::async_runtime::JoinHandle<()>,
    maintenance: tauri::async_runtime::JoinHandle<()>,
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
    pub fn spawn_background_tasks(&self) {
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
            let mut capture = CaptureLoop::new(reader, store.clone(), store.clone(), settings)
                .with_window(window)
                .with_search_cache(search_cache);
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
            let maintenance =
                MaintenanceService::new(store).with_search_cache(runtime.search_cache_handle());
            loop {
                let settings = settings_rx.borrow().clone();
                if let Err(err) = maintenance.run(&settings).await {
                    tracing::warn!(error = %err, "maintenance_failed");
                }
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    _ = settings_rx.changed() => {},
                    () = tokio::time::sleep(Duration::from_mins(30)) => {},
                }
            }
        });

        *tasks_slot = Some(BackgroundTasks {
            capture,
            maintenance,
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
            capture_reader: parts.clipboard_reader,
            background_tasks: Mutex::new(None),
            previous_frontmost: Arc::new(Mutex::new(None)),
            last_pasted_id: Mutex::new(None),
        })
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

#[cfg(test)]
mod tests {
    use std::future;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use super::*;

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
}

pub fn default_db_path() -> PathBuf {
    dirs::data_local_dir()
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
