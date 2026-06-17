//! Daemon lifecycle: `run_daemon`, OS-signal handling, the generic background
//! worker supervisor, and the graceful drain that shuts everything down.
//!
//! The IPC half of serving lives in [`super::ipc`]; this module wires the IPC
//! supervisor together with the capture / maintenance / semantic / AI-watchdog
//! / ngram workers and owns the shutdown sequencing across all of them.

use std::{sync::Arc, time::Duration};

use nagori_core::Result;
use nagori_platform::{ClipboardReader, WindowBehavior};
use tokio::{signal, sync::watch};
use tracing::{info, warn};

use super::ipc::{
    CliIpcConfig, InitialIpcState, ensure_ipc_runtime_dirs, spawn_ipc_server, spawn_ipc_supervisor,
};
use super::{POST_ABORT_JOIN_TIMEOUT, drain_one};
use crate::{CaptureLoop, MaintenanceService, NagoriRuntime, ShutdownHandle};

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// CLI IPC endpoint configuration, shared verbatim with the desktop
    /// shell so both surfaces serve byte-identical IPC.
    pub ipc: CliIpcConfig,
    pub capture_interval: Duration,
    pub maintenance_interval: Duration,
    /// Whether the capture loop's "after N AX errors, treat focus as
    /// secure" escalation is enabled. Production runs leave this `true`
    /// (the safe default); only test harnesses that can't grant
    /// Accessibility programmatically flip it to `false`. See
    /// `CaptureLoop::without_secure_focus_fail_closed`.
    pub secure_focus_fail_closed: bool,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            ipc: CliIpcConfig::default(),
            capture_interval: Duration::from_millis(500),
            maintenance_interval: Duration::from_mins(30),
            secure_focus_fail_closed: true,
        }
    }
}

/// Take the daemon's data-directory lifetime lock.
///
/// Keyed on the directory that holds the daemon's `SQLite` store, so two daemons
/// against the same DB — and a daemon vs. the desktop shell against the same
/// DB — are mutually exclusive on **every** platform: the desktop locks the
/// same directory in `AppState::try_new_at`, and the named pipe's
/// `first_pipe_instance` only ever guarded daemon-vs-daemon on Windows, not
/// the app. The caller acquires this *before* it opens the store (so a second
/// owner never runs migrations or a capture loop against a DB the first owner
/// holds) and hands it to [`run_daemon`], which holds it for the whole daemon
/// lifetime. The lock is also what makes the Unix `bind_unix_replacing_stale`
/// safe — see [`run_daemon`]. Returns the held lock, or an
/// `AppError::Platform` describing the conflict when another owner already
/// holds it. `dir` must already exist.
pub fn acquire_data_dir_lock(dir: &std::path::Path) -> Result<nagori_storage::ProcessLock> {
    match nagori_storage::ProcessLock::try_acquire(dir)? {
        Some(lock) => {
            info!(lock = %lock.path().display(), "daemon_lock_acquired");
            Ok(lock)
        }
        None => Err(nagori_core::AppError::Platform(format!(
            "another nagori daemon or the desktop app is already running and owns {}; \
             refusing to start a second instance",
            dir.display()
        ))),
    }
}

/// Initial delay before restarting a background worker that exited
/// unexpectedly. Doubles on each consecutive failure up to
/// [`WORKER_RESTART_BACKOFF_MAX`], so a worker that panics on every start
/// (a deterministic bug) backs off instead of spinning.
const WORKER_RESTART_BACKOFF_INITIAL: Duration = Duration::from_millis(250);
/// Cap on the worker restart backoff.
const WORKER_RESTART_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Restart policy for a supervised background worker.
#[derive(Clone, Copy)]
pub enum WorkerRestart {
    /// Long-running loop (capture / maintenance / semantic index): any exit
    /// while shutdown has *not* been requested is unexpected — a panic, or a
    /// settings/`watch` channel that closed early — and triggers a
    /// backoff-restart. Only the shutdown signal stops it for good.
    OnExit,
    /// One-shot pass (the ngram backfill): a clean completion is terminal and
    /// must not respawn; only a panic restarts it so the backlog still drains.
    OnPanic,
}

/// Keep a background worker alive under the daemon's lifetime.
///
/// (Re)spawns the worker through `spawn`, and on an unexpected exit — a panic,
/// or (for [`WorkerRestart::OnExit`]) any return while shutdown was not
/// requested — logs it and restarts after an exponential backoff. On shutdown
/// it drains the live worker within `grace`, then returns. A worker past
/// `grace` is aborted: that drops the supervisor's await of it so shutdown is
/// bounded, but — like the pre-existing `drain_workers` abort — it cannot stop
/// an already-running `spawn_blocking` call, which keeps running detached until
/// the syscall returns (see [`POST_ABORT_JOIN_TIMEOUT`]).
///
/// This mirrors [`supervise_ipc_server`]: before this, a panicking or
/// early-returning capture / maintenance / semantic / ngram worker was
/// invisible — `run_daemon` kept waiting on shutdown while the worker stayed
/// dead and its health snapshot went stale or falsely healthy.
///
/// Exposed so the desktop shell, which drives the same [`NagoriRuntime`]
/// workers without the CLI daemon's serve loop, can place its in-process
/// capture / maintenance / semantic / ngram / AI-watchdog tasks under the
/// identical respawn-and-drain policy.
pub async fn supervise_worker<F>(
    name: &'static str,
    restart: WorkerRestart,
    mut shutdown: ShutdownHandle,
    grace: Duration,
    mut spawn: F,
) where
    F: FnMut(ShutdownHandle) -> tokio::task::JoinHandle<()>,
{
    let mut backoff = WORKER_RESTART_BACKOFF_INITIAL;
    loop {
        let mut handle = spawn(shutdown.clone());
        tokio::select! {
            // `biased` so a real shutdown beats a coincident worker exit and we
            // don't restart a worker we're about to tear down.
            biased;
            () = shutdown.cancelled() => {
                // The worker observes the same shutdown signal, so it should
                // return between ticks; `drain_one` bounds the wait and aborts a
                // worker still running past `grace` (a detached `spawn_blocking`
                // it left behind may outlive the abort — bounded shutdown, not a
                // hard stop of in-flight blocking work).
                drain_one(name, handle, grace).await;
                return;
            }
            join = &mut handle => {
                match join {
                    Ok(()) => {
                        if shutdown.is_cancelled() {
                            return;
                        }
                        match restart {
                            // One-shot pass finished its work — nothing to respawn.
                            WorkerRestart::OnPanic => return,
                            WorkerRestart::OnExit => {
                                warn!(worker = name, "worker_exited_unexpectedly");
                            }
                        }
                    }
                    // Only the drain path above aborts a worker, and it returns
                    // there — so a cancellation here means an external abort;
                    // treat it as shutdown rather than respawning.
                    Err(err) if err.is_cancelled() => return,
                    Err(err) if err.is_panic() => {
                        warn!(worker = name, error = %err, "worker_panicked");
                    }
                    Err(err) => {
                        warn!(worker = name, error = %err, "worker_join_failed");
                    }
                }
            }
        }
        // Back off before the restart, racing shutdown so we don't sleep through
        // it (and resurrect a worker the daemon is tearing down).
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(backoff) => {}
        }
        backoff = backoff.saturating_mul(2).min(WORKER_RESTART_BACKOFF_MAX);
    }
}

/// Supervise the clipboard capture loop. `reader` is shared (`Arc`) so each
/// (re)spawn after a panic rebuilds a fresh [`CaptureLoop`] over the same
/// adapter.
fn spawn_capture_supervisor(
    runtime: NagoriRuntime,
    reader: Arc<dyn ClipboardReader>,
    window: Option<Arc<dyn WindowBehavior>>,
    config: &DaemonConfig,
    settings_rx: watch::Receiver<nagori_core::AppSettings>,
    shutdown: ShutdownHandle,
) -> tokio::task::JoinHandle<()> {
    let interval = config.capture_interval;
    let secure_focus_fail_closed = config.secure_focus_fail_closed;
    let grace = config.ipc.shutdown_grace;
    let store = runtime.store().clone();
    tokio::spawn(async move {
        supervise_worker(
            "capture",
            WorkerRestart::OnExit,
            shutdown,
            grace,
            move |mut worker_shutdown| {
                let reader = reader.clone();
                let store = store.clone();
                let window = window.clone();
                let settings_rx = settings_rx.clone();
                let search_cache = runtime.search_cache_handle();
                let capture_health = runtime.capture_health();
                let notify_runtime = runtime.clone();
                tokio::spawn(async move {
                    let settings = settings_rx.borrow().clone();
                    let semantic_notifier: Arc<dyn Fn(nagori_core::EntryId) + Send + Sync> =
                        Arc::new(move |_entry_id| notify_runtime.notify_semantic_capture());
                    let mut capture =
                        CaptureLoop::new(reader, store.clone(), store.clone(), settings)
                            .with_search_cache(search_cache)
                            .with_capture_health(capture_health)
                            .with_capture_notifier(semantic_notifier);
                    if !secure_focus_fail_closed {
                        capture = capture.without_secure_focus_fail_closed();
                    }
                    if let Some(w) = window {
                        capture = capture.with_window(w);
                    }
                    let shutdown_signal = async move { worker_shutdown.cancelled().await };
                    if let Err(err) = capture
                        .run_polling_with_settings(interval, settings_rx, shutdown_signal)
                        .await
                    {
                        warn!(error = %err, "capture_loop_terminated");
                    }
                })
            },
        )
        .await;
    })
}

/// The settings fields a maintenance sweep actually reads, so the loop can tell
/// a retention change (re-run now) from an unrelated settings edit (keep
/// waiting). Mirrors every knob `MaintenanceService::run` consults; add a field
/// here whenever the sweep starts depending on a new one, or a retention change
/// would be silently ignored until the next interval tick.
#[derive(Clone, Copy, PartialEq, Eq)]
struct RetentionKnobs {
    history_retention_count: usize,
    history_retention_days: Option<u32>,
    max_total_bytes: Option<u64>,
    max_thumbnail_total_bytes: Option<u64>,
}

impl From<&nagori_core::AppSettings> for RetentionKnobs {
    fn from(settings: &nagori_core::AppSettings) -> Self {
        Self {
            history_retention_count: settings.history_retention_count,
            history_retention_days: settings.history_retention_days,
            max_total_bytes: settings.max_total_bytes,
            max_thumbnail_total_bytes: settings.max_thumbnail_total_bytes,
        }
    }
}

/// Supervise the periodic maintenance loop (retention sweep).
fn spawn_maintenance_supervisor(
    runtime: NagoriRuntime,
    interval: Duration,
    grace: Duration,
    settings_rx: watch::Receiver<nagori_core::AppSettings>,
    shutdown: ShutdownHandle,
) -> tokio::task::JoinHandle<()> {
    let store = runtime.store().clone();
    tokio::spawn(async move {
        supervise_worker(
            "maintenance",
            WorkerRestart::OnExit,
            shutdown,
            grace,
            move |mut worker_shutdown| {
                let store = store.clone();
                let mut settings_rx = settings_rx.clone();
                let search_cache = runtime.search_cache_handle();
                let health = runtime.maintenance_health();
                tokio::spawn(async move {
                    let maintenance =
                        MaintenanceService::new(store).with_search_cache(search_cache);
                    loop {
                        let settings = settings_rx.borrow().clone();
                        match maintenance.run(&settings).await {
                            Ok(_) => health.record_success(),
                            Err(err) => {
                                // Record before logging so a concurrent
                                // health-probe sees the latest counter even if
                                // tracing back-pressure delays the warn line.
                                health.record_failure(err.to_string());
                                warn!(error = %err, "maintenance_failed");
                            }
                        }
                        let applied_retention = RetentionKnobs::from(&settings);
                        // Wait for the next trigger: shutdown, the periodic
                        // interval, or a settings change that actually moves a
                        // retention knob. A change to an unrelated field (hotkey,
                        // locale, AI toggles) used to fall straight through to
                        // another full sweep — including the VACUUM-threshold
                        // probe and a writer-lock-holding scan — for no benefit,
                        // since `MaintenanceService::run` only reads the
                        // retention fields. The sleep is pinned outside the inner
                        // loop so a burst of unrelated changes can't keep
                        // resetting the interval and starve the periodic sweep.
                        let sleep = tokio::time::sleep(interval);
                        tokio::pin!(sleep);
                        loop {
                            tokio::select! {
                                () = worker_shutdown.cancelled() => return,
                                () = &mut sleep => break,
                                changed = settings_rx.changed() => {
                                    if changed.is_err() {
                                        return;
                                    }
                                    if RetentionKnobs::from(&*settings_rx.borrow())
                                        != applied_retention
                                    {
                                        break;
                                    }
                                },
                            }
                        }
                    }
                })
            },
        )
        .await;
    })
}

/// Supervise the AI stale-request watchdog. Sweeps expired request handles on a
/// tight cadence (independent of the maintenance loop) so a leaked or wedged
/// stream's concurrency permit is reclaimed promptly rather than on the
/// 30-minute maintenance tick.
fn spawn_ai_watchdog_supervisor(
    runtime: NagoriRuntime,
    grace: Duration,
    shutdown: ShutdownHandle,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        supervise_worker(
            "ai_watchdog",
            WorkerRestart::OnExit,
            shutdown,
            grace,
            move |worker_shutdown| {
                let runtime = runtime.clone();
                tokio::spawn(async move { runtime.run_ai_request_watchdog(worker_shutdown).await })
            },
        )
        .await;
    })
}

/// Supervise the background semantic-index worker.
fn spawn_semantic_supervisor(
    runtime: NagoriRuntime,
    grace: Duration,
    shutdown: ShutdownHandle,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        supervise_worker(
            "semantic",
            WorkerRestart::OnExit,
            shutdown,
            grace,
            move |worker_shutdown| {
                let runtime = runtime.clone();
                tokio::spawn(async move { runtime.run_semantic_indexer(worker_shutdown).await })
            },
        )
        .await;
    })
}

/// Supervise the one-shot ngram-rebuild backfill. A clean completion is
/// terminal (the backlog drained); only a panic respawns it.
fn spawn_ngram_rebuild_supervisor(
    runtime: NagoriRuntime,
    grace: Duration,
    shutdown: ShutdownHandle,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        supervise_worker(
            "ngram_rebuild",
            WorkerRestart::OnPanic,
            shutdown,
            grace,
            move |worker_shutdown| {
                let runtime = runtime.clone();
                tokio::spawn(async move { runtime.run_ngram_rebuild(worker_shutdown).await })
            },
        )
        .await;
    })
}

/// Run the daemon until shutdown.
///
/// `instance_lock` is the data-directory lifetime lock the caller acquired
/// (via [`acquire_data_dir_lock`]) **before** opening the store. `run_daemon`
/// owns it for its whole body, so it is held for the daemon's lifetime and
/// released by the kernel on exit — including a crash. Holding it is what
/// makes the Unix `bind_unix_replacing_stale` below safe: the lock, not a
/// fragile `connect()` probe, proves no peer daemon is alive, so a leftover
/// socket inode is known-stale and can be replaced. It is also the
/// single-instance gate shared with the desktop shell (which locks the same
/// directory), so a standalone daemon and the app refuse to co-own one store.
#[allow(clippy::too_many_lines)]
pub async fn run_daemon<R>(
    runtime: NagoriRuntime,
    reader: R,
    config: DaemonConfig,
    window: Option<Arc<dyn WindowBehavior>>,
    instance_lock: nagori_storage::ProcessLock,
) -> Result<()>
where
    R: ClipboardReader + 'static,
{
    // Bind the lock to a lifetime-spanning local so it is held until this
    // function returns (and then dropped, releasing it). Never read otherwise.
    let _instance_lock = instance_lock;
    let shutdown = runtime.shutdown_handle();
    // Fail closed: refuse to start if the persisted settings can't be loaded
    // — running on `Default` means we'd ignore the user's denylist /
    // secret_handling / cli_ipc_enabled / capture_enabled and silently
    // re-enable a more permissive policy.
    runtime.refresh_settings_from_store().await?;
    let settings_rx = runtime.settings_subscribe();
    ensure_ipc_runtime_dirs(&config.ipc)?;

    // Every long-running worker runs under a supervisor that restarts it after
    // a panic or unexpected early return (capture / maintenance / semantic) and
    // drains it on shutdown. The ngram backfill is one-shot, so its supervisor
    // only respawns on a panic, never after the backlog drains. Without this a
    // worker that crashed left the daemon serving with a dead loop and a stale
    // health snapshot.
    //
    // `reader` becomes shared so a respawn can rebuild a fresh capture loop over
    // the same adapter.
    let reader: Arc<dyn ClipboardReader> = Arc::new(reader);
    let grace = config.ipc.shutdown_grace;

    // `refresh_settings_from_store` already succeeded above, so the daemon's
    // pre-poll init is healthy by definition — record it here (once, before the
    // capture supervisor spawns) so `nagori doctor` doesn't transiently report
    // "not ready" while the capture task is being spawned.
    runtime.startup_health().record_capture_ready();
    let capture_handle = spawn_capture_supervisor(
        runtime.clone(),
        reader,
        window,
        &config,
        settings_rx.clone(),
        shutdown.clone(),
    );
    let maintenance_handle = spawn_maintenance_supervisor(
        runtime.clone(),
        config.maintenance_interval,
        grace,
        settings_rx.clone(),
        shutdown.clone(),
    );
    let semantic_handle = spawn_semantic_supervisor(runtime.clone(), grace, shutdown.clone());
    let ai_watchdog_handle = spawn_ai_watchdog_supervisor(runtime.clone(), grace, shutdown.clone());
    // One-shot backfill that regenerates ngrams left stale by a generator
    // upgrade (kana folding / Han 1-grams). Spawned after serving has started
    // so it never blocks daemon startup; it drains the backlog in small batches
    // and then exits.
    let ngram_rebuild_handle =
        spawn_ngram_rebuild_supervisor(runtime.clone(), grace, shutdown.clone());

    // Unlike the desktop host, an initial bind failure is fatal here: a
    // headless daemon whose whole purpose is serving IPC should refuse to
    // start half-alive rather than retry in the background. Fatal must not
    // mean leaky, though — the worker supervisors above are already running,
    // so a bare `?` here would strand the capture / maintenance / semantic /
    // watchdog / ngram tasks mid-write with no shutdown signal. Cancel and
    // drain them first, then propagate the bind error.
    let initial_ipc_state = if runtime.current_settings().cli_ipc_enabled {
        match spawn_ipc_server(runtime.clone(), &config.ipc, shutdown.clone()) {
            Ok(server) => InitialIpcState::Running(server),
            Err(err) => {
                shutdown.cancel();
                drain_worker_supervisors(
                    capture_handle,
                    maintenance_handle,
                    semantic_handle,
                    ai_watchdog_handle,
                    ngram_rebuild_handle,
                    grace,
                )
                .await;
                return Err(err);
            }
        }
    } else {
        info!("ipc_disabled_by_settings");
        InitialIpcState::Disabled
    };
    let serve_handle = spawn_ipc_supervisor(
        runtime.clone(),
        config.ipc.clone(),
        settings_rx.clone(),
        shutdown.clone(),
        initial_ipc_state,
    );

    info!(socket = %config.ipc.socket_path.display(), "daemon_started");

    let mut shutdown_wait = shutdown.clone();
    tokio::select! {
        () = shutdown_wait.cancelled() => {},
        () = ctrl_c_request() => {
            shutdown.cancel();
        }
        () = terminate_request() => {
            shutdown.cancel();
        }
    }

    info!("daemon_shutting_down");
    drain_workers(
        serve_handle,
        capture_handle,
        maintenance_handle,
        semantic_handle,
        ai_watchdog_handle,
        ngram_rebuild_handle,
        config.ipc.shutdown_grace,
    )
    .await;
    // The IPC supervisor's shutdown branch calls `stop_ipc_server`, which
    // verifies fingerprints before unlinking the socket / token file. We
    // deliberately do NOT add a final unconditional `cleanup_runtime_files`
    // here — that would race a freshly-launched daemon that re-claimed the
    // path between our shutdown signal and this point.
    Ok(())
}

/// Resolve when an interactive Ctrl-C (SIGINT) arrives.
///
/// A failure to install the handler is logged and then parks forever rather
/// than tearing the daemon down: a broken signal registration is not a request
/// to stop, and SIGTERM (see [`terminate_request`]) plus the IPC `Shutdown`
/// request remain as stop paths. Without this guard a registration error
/// resolved the `select!` arm and shut the daemon down for a reason unrelated
/// to any stop request — the same fail-open the SIGTERM listener already avoids.
async fn ctrl_c_request() {
    if let Err(err) = signal::ctrl_c().await {
        warn!(error = %err, "ctrl_c_failed");
        std::future::pending::<()>().await;
    }
}

/// Resolve when the OS asks the daemon to terminate through a channel other
/// than Ctrl-C: SIGTERM on Unix (what launchd, systemd, and a bare `kill`
/// send), the console-close / system-shutdown notifications on Windows.
/// Routing these into the same shutdown cancel as Ctrl-C makes the
/// three-stage graceful drain (in-flight DB commits, fingerprint-checked
/// socket/token removal) run on service-manager stops instead of only on
/// interactive interrupts.
///
/// A failure to install the listener is logged and then parks forever: a
/// broken listener is not a reason to stop the daemon, and Ctrl-C plus the
/// IPC `Shutdown` request remain as stop paths.
#[cfg(unix)]
async fn terminate_request() {
    match signal::unix::signal(signal::unix::SignalKind::terminate()) {
        Ok(mut term) => {
            if term.recv().await.is_some() {
                info!("sigterm_received");
                return;
            }
            // The stream can no longer yield signals; never resolve rather
            // than fabricating a termination that was not requested.
            std::future::pending::<()>().await;
        }
        Err(err) => {
            warn!(error = %err, "sigterm_listener_failed");
            std::future::pending::<()>().await;
        }
    }
}

/// Windows variant of [`terminate_request`]: `taskkill` / console close
/// deliver `CTRL_CLOSE_EVENT`, system shutdown delivers `CTRL_SHUTDOWN_EVENT`.
/// Both give the process a short OS-imposed grace budget after the handler
/// returns, so starting the drain immediately is the best use of it.
#[cfg(windows)]
async fn terminate_request() {
    let close = async {
        match signal::windows::ctrl_close() {
            Ok(mut stream) => {
                if stream.recv().await.is_some() {
                    return;
                }
                std::future::pending::<()>().await;
            }
            Err(err) => {
                warn!(error = %err, "ctrl_close_listener_failed");
                std::future::pending::<()>().await;
            }
        }
    };
    let system_shutdown = async {
        match signal::windows::ctrl_shutdown() {
            Ok(mut stream) => {
                if stream.recv().await.is_some() {
                    return;
                }
                std::future::pending::<()>().await;
            }
            Err(err) => {
                warn!(error = %err, "ctrl_shutdown_listener_failed");
                std::future::pending::<()>().await;
            }
        }
    };
    tokio::select! {
        () = close => {},
        () = system_shutdown => {},
    }
    info!("terminate_event_received");
}

/// Hosts with neither Unix signals nor the Windows console control events
/// have no extra termination channel to bridge; Ctrl-C and the IPC
/// `Shutdown` request are the only stop paths.
#[cfg(not(any(unix, windows)))]
async fn terminate_request() {
    std::future::pending::<()>().await;
}

/// Three-stage graceful shutdown:
///
/// 1. The IPC supervisor observes the shutdown notify, asks the accept
///    loop to stop, and waits for its in-flight drain + abort cleanup.
///    The outer +2 s slack keeps the supervisor alive long enough to
///    finish the inner +1 s IPC-server drain and remove runtime files.
/// 2. Each worker supervisor reads the same notify and, in its shutdown
///    branch, drains its live worker within `grace` — the worker exits
///    between ticks so a partway-through DB write commits instead of
///    being abandoned — then returns. The outer drain therefore gives a
///    supervisor `grace + POST_ABORT_JOIN_TIMEOUT` plus 1 s of slack so
///    it can finish that inner drain (including the worst case where the
///    worker is wedged and force-aborted) before we move on.
/// 3. Anything still running after its window is **explicitly** aborted
///    via `handle.abort()` and we await the resulting
///    `JoinError(cancelled)` so the task is fully cleaned up before we
///    proceed to socket / token deletion. Dropping a
///    `tokio::task::JoinHandle` only detaches it, so skipping the
///    explicit abort would let a supervisor (and its worker) race the
///    file removals below — the very class of bug the grace timeout is
///    supposed to bound.
async fn drain_workers(
    serve_handle: tokio::task::JoinHandle<()>,
    capture_handle: tokio::task::JoinHandle<()>,
    maintenance_handle: tokio::task::JoinHandle<()>,
    semantic_handle: tokio::task::JoinHandle<()>,
    ai_watchdog_handle: tokio::task::JoinHandle<()>,
    ngram_rebuild_handle: tokio::task::JoinHandle<()>,
    grace: Duration,
) {
    drain_one(
        "ipc_supervisor",
        serve_handle,
        grace + Duration::from_secs(2),
    )
    .await;
    drain_worker_supervisors(
        capture_handle,
        maintenance_handle,
        semantic_handle,
        ai_watchdog_handle,
        ngram_rebuild_handle,
        grace,
    )
    .await;
}

/// Drain the five worker supervisors (everything but the IPC supervisor).
/// Shared between the normal shutdown path ([`drain_workers`]) and the
/// startup-failure path in [`run_daemon`], where the IPC server never came
/// up but the workers are already running. The caller must have cancelled
/// the shutdown handle first.
async fn drain_worker_supervisors(
    capture_handle: tokio::task::JoinHandle<()>,
    maintenance_handle: tokio::task::JoinHandle<()>,
    semantic_handle: tokio::task::JoinHandle<()>,
    ai_watchdog_handle: tokio::task::JoinHandle<()>,
    ngram_rebuild_handle: tokio::task::JoinHandle<()>,
    grace: Duration,
) {
    // A worker supervisor's own shutdown branch runs `drain_one(worker, grace)`
    // (worst case `grace + POST_ABORT_JOIN_TIMEOUT`), so the supervisor task
    // needs more than `grace` to wind down.
    let supervisor_grace = grace + POST_ABORT_JOIN_TIMEOUT + Duration::from_secs(1);
    tokio::join!(
        drain_one("capture_supervisor", capture_handle, supervisor_grace),
        drain_one(
            "maintenance_supervisor",
            maintenance_handle,
            supervisor_grace
        ),
        drain_one("semantic_supervisor", semantic_handle, supervisor_grace),
        drain_one(
            "ai_watchdog_supervisor",
            ai_watchdog_handle,
            supervisor_grace
        ),
        drain_one(
            "ngram_rebuild_supervisor",
            ngram_rebuild_handle,
            supervisor_grace
        ),
    );
}

#[cfg(all(test, unix))]
mod tests {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use nagori_storage::SqliteStore;

    use super::super::ipc::acquire_endpoint_lock;
    use super::*;

    #[test]
    fn acquire_data_dir_lock_refuses_a_second_owner() {
        let temp = tempfile::tempdir().expect("temp dir");
        let dir = temp.path();
        // The first owner takes the lock and holds it for the rest of the test.
        let first = acquire_data_dir_lock(dir).expect("first owner should acquire the lock");
        // A second owner against the same data directory is refused rather
        // than silently allowed to co-own (and double-capture into) the store.
        let err = acquire_data_dir_lock(dir)
            .expect_err("a second owner must be refused while the first holds the lock");
        assert!(
            matches!(err, nagori_core::AppError::Platform(message) if message.contains("already running")),
            "the conflict error should name the single-instance refusal"
        );
        drop(first);
        // Releasing the first lock frees the directory for a fresh owner.
        let _second =
            acquire_data_dir_lock(dir).expect("lock should be reacquirable after release");
    }

    /// `run_daemon` spawns the worker supervisors (capture / maintenance /
    /// semantic / AI watchdog / ngram rebuild) before binding IPC, and an IPC
    /// bind failure is fatal. Fatal must not mean leaky: the failure path has
    /// to cancel the shared shutdown handle and drain those workers before
    /// propagating the error, rather than returning with five supervisors
    /// still running detached. The bind is made to lose deterministically by
    /// holding the endpoint-ownership lock from the test.
    #[tokio::test]
    async fn run_daemon_drains_workers_when_ipc_bind_fails() {
        use nagori_platform::MemoryClipboard;

        let temp = tempfile::tempdir().expect("temp dir");
        let data_dir = temp.path().join("data");
        std::fs::create_dir_all(&data_dir).expect("data dir");
        let config = DaemonConfig {
            ipc: test_ipc_config(temp.path()),
            ..DaemonConfig::default()
        };
        // Occupy the endpoint lock so the daemon's own `spawn_ipc_server`
        // loses the bind after its workers are already running.
        let _endpoint_owner =
            acquire_endpoint_lock(&config.ipc).expect("test should take the endpoint lock");

        let runtime = NagoriRuntime::builder(SqliteStore::open_memory().expect("memory store"))
            .build_for_test();
        let mut shutdown = runtime.shutdown_handle();
        let instance_lock = acquire_data_dir_lock(&data_dir).expect("data dir lock");

        let err = tokio::time::timeout(
            Duration::from_secs(10),
            run_daemon(runtime, MemoryClipboard::new(), config, None, instance_lock),
        )
        .await
        .expect("the failure path must drain the workers within the grace budget")
        .expect_err("a lost endpoint bind must be fatal for run_daemon");
        assert!(
            err.to_string().contains("endpoint")
                || matches!(err, nagori_core::AppError::Platform(_)),
            "the error should be the bind failure, got: {err}"
        );
        // The cancel is what stopped the workers; `run_daemon` returning within
        // the timeout above shows the drain completed rather than detaching.
        tokio::time::timeout(Duration::from_millis(100), shutdown.cancelled())
            .await
            .expect("the failure path must cancel the shared shutdown handle");
    }

    fn test_ipc_config(dir: &std::path::Path) -> CliIpcConfig {
        CliIpcConfig {
            socket_path: dir.join("nagori.sock"),
            token_path: dir.join("nagori.token"),
            shutdown_grace: Duration::from_millis(50),
            ..CliIpcConfig::default()
        }
    }

    #[tokio::test]
    async fn supervise_worker_restarts_after_panic() {
        use std::sync::atomic::AtomicUsize;

        let runtime = NagoriRuntime::builder(SqliteStore::open_memory().expect("memory store"))
            .build_for_test();
        let shutdown = runtime.shutdown_handle();
        let spawns = Arc::new(AtomicUsize::new(0));
        let supervisor = tokio::spawn({
            let spawns = spawns.clone();
            let shutdown = shutdown.clone();
            async move {
                supervise_worker(
                    "test",
                    WorkerRestart::OnExit,
                    shutdown,
                    Duration::from_millis(50),
                    move |mut worker_shutdown| {
                        // First run panics; every subsequent run blocks until
                        // shutdown so the supervisor settles after one restart.
                        let n = spawns.fetch_add(1, Ordering::SeqCst);
                        tokio::spawn(async move {
                            assert!(n > 0, "first supervised run panics on purpose");
                            worker_shutdown.cancelled().await;
                        })
                    },
                )
                .await;
            }
        });

        // A panicking worker must be respawned rather than left dead.
        wait_until(Duration::from_secs(2), || {
            spawns.load(Ordering::SeqCst) >= 2
        })
        .await
        .expect("a panicking worker should be respawned");

        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(2), supervisor)
            .await
            .expect("supervisor should stop after shutdown")
            .expect("supervisor should not panic");
    }

    #[tokio::test]
    async fn supervise_worker_one_shot_completion_is_terminal() {
        use std::sync::atomic::AtomicUsize;

        let runtime = NagoriRuntime::builder(SqliteStore::open_memory().expect("memory store"))
            .build_for_test();
        let shutdown = runtime.shutdown_handle();
        let spawns = Arc::new(AtomicUsize::new(0));
        let supervisor = tokio::spawn({
            let spawns = spawns.clone();
            async move {
                supervise_worker(
                    "test",
                    WorkerRestart::OnPanic,
                    shutdown,
                    Duration::from_millis(50),
                    move |_worker_shutdown| {
                        spawns.fetch_add(1, Ordering::SeqCst);
                        // Completes immediately and cleanly.
                        tokio::spawn(async {})
                    },
                )
                .await;
            }
        });

        // A one-shot worker that finishes cleanly must end the supervisor
        // without a shutdown signal and without respawning.
        tokio::time::timeout(Duration::from_secs(2), supervisor)
            .await
            .expect("one-shot supervisor returns after a clean completion")
            .expect("supervisor should not panic");
        assert_eq!(
            spawns.load(Ordering::SeqCst),
            1,
            "a clean one-shot completion must not be restarted",
        );
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
}
