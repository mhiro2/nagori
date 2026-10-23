//! Shared test fixtures for the `state` submodules. Builds an `AppState` over
//! in-memory adapters so the gate / IPC-host / shutdown tests exercise the
//! desktop wiring without a live clipboard session (a Wayland compositor on
//! Linux, which headless CI runners don't provide).

use std::sync::{Arc, Mutex};

use nagori_core::Result;
use nagori_daemon::NagoriRuntime;
use nagori_platform::WindowBehavior;
use nagori_storage::SqliteStore;

use super::AppState;
use super::HotkeyFailureCache;
use super::startup::SettingsLoadGate;

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
pub(crate) fn build_test_state() -> AppState {
    let clipboard = Arc::new(nagori_platform::MemoryClipboard::new());
    build_test_state_with(clipboard, None)
}

/// [`build_test_state`] over a caller-supplied clipboard and, when given,
/// paste controller, so a test can hold a copy mid-write or record what the
/// paste step was asked to type into.
pub(crate) fn build_test_state_with<C>(
    clipboard: Arc<C>,
    paste: Option<Arc<dyn nagori_platform::PasteController>>,
) -> AppState
where
    C: nagori_platform::ClipboardReader + nagori_platform::ClipboardWriter,
{
    use nagori_platform::UnsupportedPreviewController;

    let mut builder = NagoriRuntime::builder(SqliteStore::open_memory().expect("memory store"))
        .clipboard(clipboard.clone());
    if let Some(paste) = paste {
        builder = builder.paste(paste);
    }
    let runtime = builder.build_for_test();
    let (settings_load_tx, settings_load_rx) =
        tokio::sync::watch::channel(SettingsLoadGate::Pending);
    AppState {
        runtime,
        window: Arc::new(StubWindowBehavior),
        preview: Arc::new(UnsupportedPreviewController),
        capture_reader: clipboard,
        background_tasks: Mutex::new(None),
        palette_session: Mutex::new(super::window_focus::PaletteSession::default()),
        last_pasted_id: Mutex::new(None),
        last_hotkey_failure: Mutex::new(HotkeyFailureCache::default()),
        palette_shortcut_bound: std::sync::atomic::AtomicBool::new(true),
        inflight_search: super::InflightSearch::default(),
        instance_lock: None,
        clear_on_quit_marker: None,
        settings_load_rx,
        settings_load_tx: Mutex::new(Some(settings_load_tx)),
    }
}
