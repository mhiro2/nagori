//! The [`NagoriRuntime`] facade and its feature areas.
//!
//! The runtime is the single entry point every front-end (Tauri, IPC, CLI)
//! drives, so it accumulates many concerns. They live in submodules grouped by
//! responsibility; each adds an `impl NagoriRuntime` block for its methods:
//!
//! - [`entries`] — capture, copy/paste, list, delete, pin.
//! - [`search`] — the cached search entry point and cache handle.
//! - [`settings`] — settings read/write, onboarding markers, permission probes.
//! - [`actions`] — quick (deterministic) and model-backed AI actions.
//! - [`thumbnails`] — lazy thumbnail fetch + background generation.
//! - [`doctor`] — the GitHub release-version probe behind `nagori doctor`.
//!
//! This module keeps the struct definition, the builder, the shutdown handle,
//! and the infrastructure-handle getters that don't belong to any one feature.

use std::sync::Arc;
use std::time::Instant;

use nagori_ai::{AiActionEngine, QuickActionRunner};
use nagori_core::{AppError, AppSettings};
use nagori_ipc::IpcServerHealth;
use nagori_platform::{
    Capability, ClipboardWriter, MemoryClipboard, NO_AI_ENGINE_REASON, NoopPasteController,
    PasteController, PermissionChecker, PlatformCapabilities, unsupported_capabilities,
};
use nagori_storage::SqliteStore;
use tokio::sync::{Mutex as AsyncMutex, watch};

use crate::ai_registry::AiRequestRegistry;
use crate::health::{CaptureHealth, MaintenanceHealth, StartupHealth};
use crate::search_cache::{SharedSearchCache, new_shared_cache};
use crate::thumbnails::ThumbnailGate;

use self::doctor::UpdateProbeState;

mod actions;
mod doctor;
mod entries;
mod search;
mod settings;
mod thumbnails;

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub struct NagoriRuntime {
    pub(crate) store: SqliteStore,
    clipboard: Arc<dyn ClipboardWriter>,
    paste: Arc<dyn PasteController>,
    /// Model-backed AI engine. `None` on platforms with no wired backend
    /// (currently everything but macOS); AI actions are refused there while
    /// quick actions stay available.
    ai_engine: Option<Arc<dyn AiActionEngine>>,
    /// Deterministic rule-based quick actions, always available.
    quick_runner: Arc<QuickActionRunner>,
    /// Tracks in-flight AI actions and owns their cancellation tokens.
    ai_registry: Arc<AiRequestRegistry>,
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
    /// Shared state for the background semantic-index worker: its current
    /// coarse state, a wake signal new captures fire, a rebuild flag, and the
    /// AC-power probe its battery guard reads. See `semantic_index.rs`.
    pub(crate) semantic: Arc<crate::semantic_index::SemanticState>,
}

impl NagoriRuntime {
    pub fn builder(store: SqliteStore) -> NagoriRuntimeBuilder {
        NagoriRuntimeBuilder {
            store,
            clipboard: None,
            paste: None,
            ai_engine: None,
            permissions: None,
            socket_path: None,
            capabilities: None,
            power_probe: None,
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

    pub fn settings_subscribe(&self) -> watch::Receiver<AppSettings> {
        self.settings_rx.clone()
    }

    pub fn current_settings(&self) -> AppSettings {
        self.settings_rx.borrow().clone()
    }

    /// The wired embedding backend, if any. The semantic index pipeline drives
    /// it directly (embedding is not an `AiActionId`-level streaming action).
    pub(crate) fn embedder(&self) -> Option<Arc<dyn nagori_ai::Embedder>> {
        self.ai_engine.as_ref().and_then(|engine| engine.embedder())
    }

    /// The semaphore that bounds concurrent embedding work, shared with the
    /// registry so on-demand semantic queries and the background indexer never
    /// run two embedding passes at once.
    pub(crate) fn embedding_semaphore(&self) -> Arc<tokio::sync::Semaphore> {
        Arc::clone(&self.ai_registry.semaphores().embedding)
    }
}

/// Conservative upper bound on AI input size, in estimated tokens.
///
/// Apple's Foundation Models cap a session at 4,096 tokens (instructions +
/// prompt + output) and silently truncate on overflow, so the daemon refuses
/// input above this budget rather than letting the model drop text. The margin
/// below 4,096 leaves room for the instructions and the generated summary.
const MAX_AI_INPUT_TOKENS: usize = 3_500;

/// Saturating `Instant`-since → whole-millisecond conversion for structured
/// log fields, mirroring the desktop command layer's helper so a pathological
/// duration can't panic the narrowing.
pub(crate) fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

pub struct NagoriRuntimeBuilder {
    store: SqliteStore,
    clipboard: Option<Arc<dyn ClipboardWriter>>,
    paste: Option<Arc<dyn PasteController>>,
    ai_engine: Option<Arc<dyn AiActionEngine>>,
    permissions: Option<Arc<dyn PermissionChecker>>,
    socket_path: Option<std::path::PathBuf>,
    capabilities: Option<PlatformCapabilities>,
    power_probe: Option<crate::semantic_index::PowerProbe>,
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

    /// Wires the model-backed AI engine. Leave unset on platforms with no
    /// backend; AI actions are then refused while quick actions stay available.
    #[must_use]
    pub fn ai_engine(mut self, engine: Arc<dyn AiActionEngine>) -> Self {
        self.ai_engine = Some(engine);
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

    /// Set the AC-power probe the semantic indexer's battery guard reads.
    ///
    /// `nagori-platform-native::build_native_runtime` wires the host probe
    /// (`IOKit` on macOS); unset, the guard treats power as unknown and runs.
    #[must_use]
    pub fn power_probe(mut self, probe: crate::semantic_index::PowerProbe) -> Self {
        self.power_probe = Some(probe);
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
    /// (`MemoryClipboard`, `NoopPasteController`, and no AI engine)
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
        let mut capabilities = self.capabilities.unwrap_or_else(unsupported_capabilities);
        // Reconcile the AI capability with the *actually wired* engine so
        // the desktop's gate (and the capability matrix) can never claim
        // AI on a host with no backend, and lights up automatically on any
        // host that gains one — today macOS, a future runtime-configured
        // (e.g. OpenAI-compatible) provider tomorrow. This is the single
        // switch: wiring `ai_engine` is all it takes, with no per-OS edit
        // to the static report. Live model readiness stays on the separate
        // `AiAvailabilityReport` channel.
        capabilities.ai_actions = if self.ai_engine.is_some() {
            Capability::Available
        } else {
            Capability::Unsupported {
                reason: NO_AI_ENGINE_REASON.to_owned(),
            }
        };
        let capabilities = Arc::new(capabilities);
        NagoriRuntime {
            store: self.store,
            clipboard,
            paste,
            ai_engine: self.ai_engine,
            quick_runner: Arc::new(QuickActionRunner::new()),
            ai_registry: Arc::new(AiRequestRegistry::new()),
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
            semantic: Arc::new(crate::semantic_index::SemanticState::new(self.power_probe)),
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

    /// Non-blocking check of whether shutdown has been signalled, for loops that
    /// poll between units of work rather than `select!`-ing on `cancelled`.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.rx.borrow()
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
