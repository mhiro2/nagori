//! Startup wiring for the desktop shell: the one-shot settings-load gate, the
//! background-worker supervisors, and the in-process CLI IPC host.
//!
//! `spawn_background_tasks` is the entry point `lib.rs` calls once the state is
//! managed; everything else here is the machinery it drives (mirroring the
//! daemon's `serve/lifecycle.rs`) plus the shutdown drain.

use std::sync::Arc;
use std::time::Duration;

use nagori_core::{AppSettings, EntryId, Result};
use nagori_daemon::{
    CaptureLoop, CliIpcConfig, MaintenanceHealth, MaintenanceReport, MaintenanceService,
    NagoriRuntime, ShutdownHandle, StartupHealth, WorkerRestart, spawn_cli_ipc_supervisor,
    supervise_worker,
};
use nagori_platform::{ClipboardReader, WindowBehavior};

use super::AppState;

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

pub(super) struct BackgroundTasks {
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
            // Resolve the IPC config fail-closed: if the per-user data dir
            // can't be resolved (no HOME), refuse to serve IPC rather than
            // bind a socket and write the auth token under the working
            // directory. Mirrors the CLI's fallible token-path resolution.
            cli_ipc: match CliIpcConfig::resolve_default() {
                Ok(config) => self.spawn_cli_ipc_host(config),
                Err(err) => {
                    tracing::warn!(error = %err, "cli_ipc_token_path_unresolved_not_serving");
                    tauri::async_runtime::spawn(async {})
                }
            },
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

    /// Clone the startup settings-load gate receiver so a worker spawned
    /// outside `spawn_background_tasks` (the settings subscriber, in `lib.rs`)
    /// can await the single coordinator load via [`settings_loaded_ok`].
    pub(crate) fn settings_load_gate(&self) -> tokio::sync::watch::Receiver<SettingsLoadGate> {
        self.settings_load_rx.clone()
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
/// it does on the daemon (`serve/lifecycle.rs`). Extracted from the spawn body so the
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    use nagori_core::AppError;

    use super::*;
    use crate::state::test_support::build_test_state;

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
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

    /// Parity with the daemon's `serve/lifecycle.rs` path: feeding the same
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
}
