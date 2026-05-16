use std::{path::PathBuf, sync::Arc, time::Duration};

#[cfg(not(any(unix, windows)))]
use nagori_core::AppError;
use nagori_core::Result;
#[cfg(windows)]
use nagori_ipc::{AuthToken, accept_loop_pipe_with_shutdown, bind_pipe, write_token_file};
#[cfg(unix)]
use nagori_ipc::{
    AuthToken, accept_loop_with_shutdown, bind_unix, default_token_path, write_token_file,
};
use nagori_platform::{ClipboardReader, WindowBehavior};
use tokio::{signal, sync::watch};
use tracing::{info, warn};

use crate::{CaptureLoop, MaintenanceService, NagoriRuntime, ShutdownHandle};

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// On Unix this is a filesystem path for the Unix-domain socket. On
    /// Windows it is the named-pipe name (e.g. `\\.\pipe\nagori`) packed in
    /// a `PathBuf` so existing call-sites that store the IPC endpoint keep
    /// working without a platform-conditional type.
    pub socket_path: PathBuf,
    pub token_path: PathBuf,
    pub capture_interval: Duration,
    pub maintenance_interval: Duration,
    /// Maximum time to wait for in-flight IPC handlers to commit during
    /// shutdown before they're aborted. Picked to be longer than the
    /// slowest expected DB write (FTS index update on a large entry) but
    /// short enough that `Ctrl-C` on a stuck daemon still returns quickly.
    pub shutdown_grace: Duration,
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
            socket_path: default_socket_path(),
            token_path: default_token_path_local(),
            capture_interval: Duration::from_millis(500),
            maintenance_interval: Duration::from_mins(30),
            shutdown_grace: Duration::from_secs(5),
            secure_focus_fail_closed: true,
        }
    }
}

#[cfg(unix)]
pub fn default_socket_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("nagori")
        .join("nagori.sock")
}

#[cfg(windows)]
pub fn default_socket_path() -> PathBuf {
    PathBuf::from(nagori_ipc::DEFAULT_PIPE_NAME)
}

#[cfg(not(any(unix, windows)))]
pub fn default_socket_path() -> PathBuf {
    PathBuf::from("nagori.sock")
}

#[cfg(unix)]
fn default_token_path_local() -> PathBuf {
    default_token_path()
}

#[cfg(not(unix))]
fn default_token_path_local() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("nagori")
        .join("nagori.token")
}

/// Bind the IPC socket, mint a per-launch auth token, and spawn the accept loop.
///
/// The bind happens synchronously up-front so a failure (socket-in-use,
/// permission denied, parent dir missing) terminates `run_daemon` startup
/// instead of leaving a half-alive process. The auth token is written to a
/// sibling `0o600` file; only callers who can read that file
/// (== same user ownership as the daemon) will authenticate.
async fn spawn_ipc_server(
    runtime: NagoriRuntime,
    config: &DaemonConfig,
    shutdown: ShutdownHandle,
) -> Result<IpcServerTask> {
    let (stop_tx, stop_rx) = watch::channel(false);
    #[cfg(unix)]
    {
        let listener = bind_unix(&config.socket_path).await?;
        let token = AuthToken::generate()?;
        write_token_file(&config.token_path, &token)?;
        let grace = config.shutdown_grace;
        let handle = tokio::spawn(async move {
            let mut shutdown = shutdown;
            let mut stop_rx = stop_rx;
            let result = accept_loop_with_shutdown(
                listener,
                token,
                move |request| {
                    let runtime = runtime.clone();
                    async move { runtime.handle_ipc(request).await }
                },
                async move {
                    tokio::select! {
                        () = shutdown.cancelled() => {},
                        () = ipc_stop_requested(&mut stop_rx) => {},
                    }
                },
                grace,
            )
            .await;
            if let Err(err) = result {
                warn!(error = %err, "ipc_server_terminated");
            }
        });
        Ok(IpcServerTask { handle, stop_tx })
    }
    #[cfg(windows)]
    {
        // Bind the first pipe instance synchronously so a collision with an
        // already-running daemon (or any other process holding the same
        // pipe name) propagates out of `run_daemon` startup. If we deferred
        // the bind into `tokio::spawn`, the failure would only surface as a
        // warn line from the spawned task while the daemon kept running —
        // and we'd have already written a token file that no one is
        // serving.
        let pipe_name = config.socket_path.to_string_lossy().into_owned();
        let first_instance = bind_pipe(&pipe_name)?;
        let token = AuthToken::generate()?;
        write_token_file(&config.token_path, &token)?;
        let grace = config.shutdown_grace;
        let handle = tokio::spawn(async move {
            let mut shutdown = shutdown;
            let mut stop_rx = stop_rx;
            let result = accept_loop_pipe_with_shutdown(
                &pipe_name,
                first_instance,
                token,
                move |request| {
                    let runtime = runtime.clone();
                    async move { runtime.handle_ipc(request).await }
                },
                async move {
                    tokio::select! {
                        () = shutdown.cancelled() => {},
                        () = ipc_stop_requested(&mut stop_rx) => {},
                    }
                },
                grace,
            )
            .await;
            if let Err(err) = result {
                warn!(error = %err, "ipc_server_terminated");
            }
        });
        Ok(IpcServerTask { handle, stop_tx })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (runtime, config, shutdown, stop_tx, stop_rx);
        Err(AppError::Unsupported(
            "IPC requires a Unix-like or Windows platform".to_owned(),
        ))
    }
}

struct IpcServerTask {
    handle: tokio::task::JoinHandle<()>,
    stop_tx: watch::Sender<bool>,
}

impl IpcServerTask {
    fn request_stop(&self) {
        let _ = self.stop_tx.send_replace(true);
    }
}

async fn ipc_stop_requested(stop_rx: &mut watch::Receiver<bool>) {
    if *stop_rx.borrow_and_update() {
        return;
    }
    loop {
        if stop_rx.changed().await.is_err() {
            return;
        }
        if *stop_rx.borrow_and_update() {
            return;
        }
    }
}

async fn supervise_ipc_server(
    runtime: NagoriRuntime,
    config: DaemonConfig,
    mut settings_rx: watch::Receiver<nagori_core::AppSettings>,
    mut shutdown: ShutdownHandle,
    mut server: Option<IpcServerTask>,
) {
    loop {
        tokio::select! {
            () = shutdown.cancelled() => {
                if let Some(server) = server.take() {
                    stop_ipc_server(server, &config).await;
                }
                return;
            }
            changed = settings_rx.changed() => {
                if changed.is_err() {
                    if let Some(server) = server.take() {
                        stop_ipc_server(server, &config).await;
                    }
                    return;
                }
                let enabled = settings_rx.borrow().cli_ipc_enabled;
                match (enabled, server.is_some()) {
                    (true, false) => match spawn_ipc_server(runtime.clone(), &config, shutdown.clone()).await {
                        Ok(next) => {
                            info!(socket = %config.socket_path.display(), "ipc_server_started");
                            server = Some(next);
                        }
                        Err(err) => warn!(error = %err, "ipc_server_start_failed"),
                    },
                    (false, true) => {
                        info!("ipc_disabled_by_settings");
                        if let Some(current) = server.take() {
                            stop_ipc_server(current, &config).await;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn stop_ipc_server(server: IpcServerTask, config: &DaemonConfig) {
    server.request_stop();
    drain_one(
        "ipc_serve",
        server.handle,
        config.shutdown_grace + Duration::from_secs(1),
    )
    .await;
    cleanup_runtime_files(config);
}

fn spawn_ipc_supervisor(
    runtime: NagoriRuntime,
    config: DaemonConfig,
    settings_rx: watch::Receiver<nagori_core::AppSettings>,
    shutdown: ShutdownHandle,
    initial_server: Option<IpcServerTask>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        supervise_ipc_server(runtime, config, settings_rx, shutdown, initial_server).await;
    })
}

fn ensure_ipc_runtime_dirs(config: &DaemonConfig) -> Result<()> {
    // On Windows the socket_path is a pipe name (e.g. `\\.\pipe\nagori`),
    // not a filesystem path. Only ensure the parent directory exists when
    // we actually need a filesystem-resident IPC endpoint.
    #[cfg(unix)]
    if let Some(parent) = config.socket_path.parent() {
        nagori_storage::ensure_private_directory(parent)?;
    }
    // The token file is always filesystem-backed (Windows daemon writes to
    // `%LOCALAPPDATA%\nagori\nagori.token`), so ensure that directory exists
    // on every platform.
    if let Some(parent) = config.token_path.parent() {
        nagori_storage::ensure_private_directory(parent)?;
    }
    Ok(())
}

pub async fn run_daemon<R>(
    runtime: NagoriRuntime,
    reader: R,
    config: DaemonConfig,
    window: Option<Arc<dyn WindowBehavior>>,
) -> Result<()>
where
    R: ClipboardReader + 'static,
{
    let store = runtime.store().clone();
    let shutdown = runtime.shutdown_handle();
    // Fail closed: refuse to start if the persisted settings can't be loaded
    // — running on `Default` means we'd ignore the user's denylist /
    // secret_handling / cli_ipc_enabled / capture_enabled and silently
    // re-enable a more permissive policy.
    runtime.refresh_settings_from_store().await?;
    let settings_rx = runtime.settings_subscribe();
    ensure_ipc_runtime_dirs(&config)?;

    let capture_handle = {
        let store = store.clone();
        let mut shutdown = shutdown.clone();
        let interval = config.capture_interval;
        let settings_rx = settings_rx.clone();
        let window = window.clone();
        let search_cache = runtime.search_cache_handle();
        let secure_focus_fail_closed = config.secure_focus_fail_closed;
        tokio::spawn(async move {
            let settings = settings_rx.borrow().clone();
            let mut capture = CaptureLoop::new(reader, store.clone(), store.clone(), settings)
                .with_search_cache(search_cache);
            if !secure_focus_fail_closed {
                capture = capture.without_secure_focus_fail_closed();
            }
            if let Some(w) = window {
                capture = capture.with_window(w);
            }
            let shutdown_signal = async move { shutdown.cancelled().await };
            if let Err(err) = capture
                .run_polling_with_settings(interval, settings_rx, shutdown_signal)
                .await
            {
                warn!(error = %err, "capture_loop_terminated");
            }
        })
    };

    let maintenance_handle = {
        let store = store.clone();
        let mut shutdown = shutdown.clone();
        let interval = config.maintenance_interval;
        let mut settings_rx = settings_rx.clone();
        let search_cache = runtime.search_cache_handle();
        let health = runtime.maintenance_health();
        tokio::spawn(async move {
            let maintenance =
                MaintenanceService::new(store.clone()).with_search_cache(search_cache);
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
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    changed = settings_rx.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    },
                    () = tokio::time::sleep(interval) => {},
                }
            }
        })
    };

    let initial_ipc_server = if runtime.current_settings().cli_ipc_enabled {
        Some(spawn_ipc_server(runtime.clone(), &config, shutdown.clone()).await?)
    } else {
        info!("ipc_disabled_by_settings");
        None
    };
    let serve_handle = spawn_ipc_supervisor(
        runtime.clone(),
        config.clone(),
        settings_rx.clone(),
        shutdown.clone(),
        initial_ipc_server,
    );

    info!(socket = %config.socket_path.display(), "daemon_started");

    let mut shutdown_wait = shutdown.clone();
    tokio::select! {
        () = shutdown_wait.cancelled() => {},
        result = signal::ctrl_c() => {
            if let Err(err) = result {
                warn!(error = %err, "ctrl_c_failed");
            }
            shutdown.cancel();
        }
    }

    info!("daemon_shutting_down");
    drain_workers(
        serve_handle,
        capture_handle,
        maintenance_handle,
        config.shutdown_grace,
    )
    .await;
    cleanup_runtime_files(&config);
    Ok(())
}

/// Three-stage graceful shutdown:
///
/// 1. The IPC supervisor observes the shutdown notify, asks the accept
///    loop to stop, and waits for its in-flight drain + abort cleanup.
///    The outer +2 s slack keeps the supervisor alive long enough to
///    finish the inner +1 s IPC-server drain and remove runtime files.
/// 2. Capture + maintenance loops read the same notify and exit between
///    ticks; we give them up to `grace` to finish the current iteration
///    so a partway-through DB write commits instead of being abandoned.
/// 3. Anything still running after `grace` is **explicitly** aborted via
///    `handle.abort()` and we await the resulting `JoinError(cancelled)`
///    so the task is fully cleaned up before we proceed to socket / token
///    deletion. Dropping a `tokio::task::JoinHandle` only detaches it, so
///    skipping the explicit abort would let capture / maintenance / IPC
///    workers race the file removals below — the very class of bug the
///    grace timeout is supposed to bound.
async fn drain_workers(
    serve_handle: tokio::task::JoinHandle<()>,
    capture_handle: tokio::task::JoinHandle<()>,
    maintenance_handle: tokio::task::JoinHandle<()>,
    grace: Duration,
) {
    drain_one(
        "ipc_supervisor",
        serve_handle,
        grace + Duration::from_secs(2),
    )
    .await;
    tokio::join!(
        drain_one("capture", capture_handle, grace),
        drain_one("maintenance", maintenance_handle, grace),
    );
}

/// Borrow-then-abort drain. We `&mut handle` so the timeout doesn't move
/// the handle out of scope: on the timeout branch we still have it to
/// call `abort()` on, then await again so the cancellation completes
/// before we return.
async fn drain_one(name: &'static str, mut handle: tokio::task::JoinHandle<()>, grace: Duration) {
    match tokio::time::timeout(grace, &mut handle).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => warn!(error = %err, worker = name, "drain_join_failed"),
        Err(_) => {
            warn!(worker = name, "drain_timeout_aborting");
            handle.abort();
            // The post-abort await yields a `JoinError(cancelled)` on the
            // common path; treat both Ok and Err as "task is done" and
            // only log unexpected panics.
            match handle.await {
                Ok(()) => {}
                Err(err) if err.is_cancelled() => {}
                Err(err) => warn!(error = %err, worker = name, "drain_abort_join_failed"),
            }
        }
    }
}

fn cleanup_runtime_files(config: &DaemonConfig) {
    // On Windows `socket_path` is a pipe name and `exists()` will report
    // false (the pipe namespace isn't a filesystem); the check + remove
    // become harmless no-ops. On Unix this unlinks the lingering socket
    // inode (we held the listener open until shutdown).
    if config.socket_path.exists()
        && let Err(err) = std::fs::remove_file(&config.socket_path)
    {
        warn!(error = %err, path = %config.socket_path.display(), "socket_cleanup_failed");
    }
    // Remove the token file on shutdown so a CLI launched after the daemon
    // exits gets a clean "no daemon running" error instead of trying a
    // stale token against a fresh process.
    if config.token_path.exists()
        && let Err(err) = std::fs::remove_file(&config.token_path)
    {
        warn!(error = %err, path = %config.token_path.display(), "token_cleanup_failed");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::{Duration, Instant};

    use nagori_core::AppSettings;
    use nagori_ipc::{IpcClient, IpcRequest, IpcResponse};
    use nagori_storage::SqliteStore;

    use super::*;

    #[tokio::test]
    async fn settings_change_stops_existing_ipc_server() {
        let temp = tempfile::tempdir().expect("temp dir");
        let config = DaemonConfig {
            socket_path: temp.path().join("nagori.sock"),
            token_path: temp.path().join("nagori.token"),
            shutdown_grace: Duration::from_millis(50),
            ..DaemonConfig::default()
        };
        let runtime = NagoriRuntime::builder(SqliteStore::open_memory().expect("memory store"))
            .build_for_test();
        let settings_rx = runtime.settings_subscribe();
        let shutdown = runtime.shutdown_handle();
        let initial_server = spawn_ipc_server(runtime.clone(), &config, shutdown.clone())
            .await
            .expect("IPC server should start");
        let supervisor = tokio::spawn(supervise_ipc_server(
            runtime.clone(),
            config.clone(),
            settings_rx,
            shutdown.clone(),
            Some(initial_server),
        ));
        let token = nagori_ipc::read_token_file(&config.token_path).expect("token file");
        let client = IpcClient::new(config.socket_path.to_string_lossy().into_owned(), token)
            .with_connect_timeout(Duration::from_millis(100))
            .with_request_timeout(Duration::from_secs(1));

        let health = client
            .send(IpcRequest::Health)
            .await
            .expect("health before disable");
        assert!(matches!(health, IpcResponse::Health(_)));

        runtime
            .save_settings(AppSettings {
                cli_ipc_enabled: false,
                ..AppSettings::default()
            })
            .await
            .expect("save settings");
        wait_until(Duration::from_secs(1), || !config.socket_path.exists())
            .await
            .expect("socket should be removed after disable");
        assert!(
            !config.token_path.exists(),
            "token file should be removed after disable",
        );

        let err = client
            .send(IpcRequest::Health)
            .await
            .expect_err("disabled IPC should refuse new connections");
        assert!(matches!(err, nagori_core::AppError::Platform(_)));

        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(1), supervisor)
            .await
            .expect("supervisor should stop")
            .expect("supervisor should not panic");
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
