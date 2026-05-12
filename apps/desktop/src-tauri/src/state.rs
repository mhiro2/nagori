use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;
#[cfg(target_os = "macos")]
use std::{sync::Arc, time::Duration};

#[cfg(not(target_os = "macos"))]
use std::time::Duration;

/// How long a "last pasted" entry id stays valid before it falls back to
/// the recency head. Picked at 30 min so a short break between pastes
/// (coffee, meeting) still threads back to the same clip, but a fresh
/// session many hours later doesn't surface a context-mismatched paste
/// from a different task.
const LAST_PASTED_TTL: Duration = Duration::from_mins(30);

use nagori_ai::LocalAiProvider;
#[cfg(target_os = "macos")]
use nagori_core::SourceApp;
use nagori_core::{AppError, EntryId, Result};
use nagori_daemon::NagoriRuntime;
#[cfg(target_os = "macos")]
use nagori_daemon::{CaptureLoop, MaintenanceService};
use nagori_storage::SqliteStore;

#[cfg(target_os = "macos")]
use nagori_platform::WindowBehavior;
#[cfg(target_os = "macos")]
use nagori_platform_macos::{
    MacosClipboard, MacosPasteController, MacosPermissionChecker, MacosWindowBehavior,
};

pub struct AppState {
    pub runtime: NagoriRuntime,
    #[cfg(target_os = "macos")]
    pub window: Arc<dyn WindowBehavior>,
    #[cfg(target_os = "macos")]
    background_tasks: Mutex<Option<BackgroundTasks>>,
    /// Frontmost app captured the last time the palette was opened.
    /// Used by `paste_entry` to re-focus the user's prior app before
    /// synthesising ⌘V — without it, the keystroke lands on the
    /// (still-focused) Nagori webview and we paste into our own search
    /// box.
    #[cfg(target_os = "macos")]
    pub previous_frontmost: Arc<Mutex<Option<SourceApp>>>,
    /// Most recently pasted entry id, paired with the `Instant` it was
    /// recorded. Powers the "repaste last" secondary hotkey so it
    /// targets the entry the user actually pasted instead of whatever
    /// happens to top the recency list (a fresh capture from elsewhere
    /// can otherwise hijack the slot between pastes). The timestamp
    /// drives TTL expiry — see `LAST_PASTED_TTL` — so a paste recorded
    /// hours ago doesn't silently resurface in a new working context.
    pub last_pasted_id: Mutex<Option<(EntryId, Instant)>>,
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
impl AppState {
    /// Snapshot the current frontmost app and store it as the "previous
    /// frontmost" — call this immediately *before* showing the palette so
    /// the snapshot reflects the source the user copied from / wants to
    /// paste back into.
    pub fn remember_previous_frontmost(&self) {
        let snapshot = MacosWindowBehavior::frontmost_app_blocking().map(|front| front.source);
        if let Ok(mut slot) = self.previous_frontmost.lock() {
            *slot = snapshot;
        }
    }

    pub fn take_previous_frontmost(&self) -> Option<SourceApp> {
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
        #[cfg(target_os = "macos")]
        {
            Self::build(store)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(Self::build(store))
        }
    }

    /// Spawns the in-process capture loop and a low-frequency maintenance
    /// loop. Call once after `manage(state)` so a Tokio runtime is available.
    #[cfg(target_os = "macos")]
    pub fn spawn_background_tasks(&self) {
        let mut tasks_slot = self.background_tasks_slot();
        if tasks_slot.is_some() {
            tracing::warn!("background_tasks_already_started");
            return;
        }

        let runtime = self.runtime.clone();
        let window = self.window.clone();
        let search_cache = self.runtime.search_cache_handle();
        let capture = tauri::async_runtime::spawn(async move {
            // Fail closed: refuse to start the capture loop if the persisted
            // settings cannot be loaded — running with `Default` would drop
            // the user's denylist / regex_denylist / secret_handling and
            // capture more aggressively than configured.
            if let Err(err) = runtime.refresh_settings_from_store().await {
                tracing::error!(error = %err, "settings_load_failed_aborting_capture");
                return;
            }
            let store = runtime.store().clone();
            let settings = runtime.current_settings();
            let reader = match MacosClipboard::new() {
                Ok(reader) => reader,
                Err(err) => {
                    tracing::warn!(error = %err, "clipboard_reader_unavailable");
                    return;
                }
            };
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
    #[cfg(target_os = "macos")]
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

    #[cfg(target_os = "macos")]
    fn background_tasks_slot(&self) -> std::sync::MutexGuard<'_, Option<BackgroundTasks>> {
        self.background_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(target_os = "macos")]
    fn build(store: SqliteStore) -> Result<Self> {
        let clipboard = Arc::new(MacosClipboard::new()?);
        let window: Arc<dyn WindowBehavior> = Arc::new(MacosWindowBehavior::new());
        let runtime = NagoriRuntime::builder(store)
            .clipboard(clipboard)
            .paste(Arc::new(MacosPasteController))
            .ai(Arc::new(LocalAiProvider::default()))
            .permissions(Arc::new(MacosPermissionChecker))
            .build();
        Ok(Self {
            runtime,
            window,
            background_tasks: Mutex::new(None),
            previous_frontmost: Arc::new(Mutex::new(None)),
            last_pasted_id: Mutex::new(None),
        })
    }

    #[cfg(not(target_os = "macos"))]
    fn build(store: SqliteStore) -> Self {
        use std::sync::Arc;
        let runtime = NagoriRuntime::builder(store)
            .ai(Arc::new(LocalAiProvider::default()))
            .build();
        Self {
            runtime,
            last_pasted_id: Mutex::new(None),
        }
    }
}

#[cfg(target_os = "macos")]
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

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::{
        future,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
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
