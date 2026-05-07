use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::sync::Mutex;
#[cfg(target_os = "macos")]
use std::{sync::Arc, time::Duration};

use nagori_ai::LocalAiProvider;
#[cfg(target_os = "macos")]
use nagori_core::SourceApp;
use nagori_core::{AppError, Result};
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
    /// Frontmost app captured the last time the palette was opened.
    /// Used by `paste_entry` to re-focus the user's prior app before
    /// synthesising ⌘V — without it, the keystroke lands on the
    /// (still-focused) Nagori webview and we paste into our own search
    /// box.
    #[cfg(target_os = "macos")]
    pub previous_frontmost: Arc<Mutex<Option<SourceApp>>>,
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
        let runtime = self.runtime.clone();
        let window = self.window.clone();
        let search_cache = self.runtime.search_cache_handle();
        tauri::async_runtime::spawn(async move {
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
            let shutdown = runtime.shutdown_handle();
            let shutdown_signal = async move { shutdown.notified().await };
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
        tauri::async_runtime::spawn(async move {
            let store = runtime.store().clone();
            let mut settings_rx = runtime.settings_subscribe();
            let shutdown = runtime.shutdown_handle();
            let maintenance =
                MaintenanceService::new(store).with_search_cache(runtime.search_cache_handle());
            loop {
                let settings = settings_rx.borrow().clone();
                if let Err(err) = maintenance.run(&settings).await {
                    tracing::warn!(error = %err, "maintenance_failed");
                }
                tokio::select! {
                    () = shutdown.notified() => return,
                    _ = settings_rx.changed() => {},
                    () = tokio::time::sleep(Duration::from_mins(30)) => {},
                }
            }
        });
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
            previous_frontmost: Arc::new(Mutex::new(None)),
        })
    }

    #[cfg(not(target_os = "macos"))]
    fn build(store: SqliteStore) -> Self {
        use std::sync::Arc;
        let runtime = NagoriRuntime::builder(store)
            .ai(Arc::new(LocalAiProvider::default()))
            .build();
        Self { runtime }
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
