use std::{
    num::NonZeroUsize,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

#[cfg(not(any(unix, windows)))]
use nagori_core::AppError;
use nagori_core::Result;
use nagori_ipc::IpcServerConfig;
#[cfg(windows)]
use nagori_ipc::{AuthToken, accept_loop_pipe_with_shutdown, bind_pipe, write_token_file};
#[cfg(unix)]
use nagori_ipc::{
    AuthToken, accept_loop_with_shutdown, bind_unix_replacing_stale, default_token_path,
    write_token_file,
};
use nagori_platform::{ClipboardReader, WindowBehavior};
use tokio::{signal, sync::watch};
use tracing::{info, warn};

use crate::{CaptureLoop, MaintenanceService, NagoriRuntime, ShutdownHandle};

/// Initial delay between unexpected accept-loop exits before retrying. Doubles
/// on each consecutive failure up to [`IPC_RESTART_BACKOFF_MAX`].
const IPC_RESTART_BACKOFF_INITIAL: Duration = Duration::from_millis(250);
/// Cap so a persistently-failing bind doesn't blow out the supervisor's
/// retry interval into hours.
const IPC_RESTART_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// How often the supervisor self-probes the IPC endpoint to confirm the
/// accept loop is still firing. Kept well above the per-connection
/// `READ_TIMEOUT` so a probe never collides with an in-flight handler's
/// own teardown, but small enough that a wedge surfaces within a couple
/// of minutes rather than waiting for the next external client.
const IPC_LIVENESS_PROBE_INTERVAL: Duration = Duration::from_secs(30);

/// Maximum age of `IpcServerHealth::last_accept_at_ms` before the
/// supervisor treats the accept loop as wedged and aborts the server
/// task. Chosen as three probe intervals so a single slow probe (or one
/// transient connect failure) does not trigger a restart by itself.
const IPC_LIVENESS_WEDGE_THRESHOLD: Duration = Duration::from_secs(90);

/// Bound on the supervisor's self-probe so a wedged listener (or a
/// kernel that hung the connect itself) cannot park the supervisor for
/// the full probe interval. One second is plenty of slack for a local
/// `connect()`; the wedge detector measures freshness of accept rather
/// than the probe's own success, so a timed-out probe still lets the
/// staleness check decide whether to abort.
const IPC_LIVENESS_PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// Grace window between issuing the probe and re-reading
/// `last_accept_at_ms`. The accept arm bumps the timestamp before it
/// touches the semaphore, so a healthy loop lands the update inside
/// this window even when the handler pool is saturated.
const IPC_LIVENESS_PROBE_SETTLE: Duration = Duration::from_millis(200);

/// Everything the CLI IPC server needs to bind, authenticate, and drain.
///
/// Split out of [`DaemonConfig`] so a host that only serves IPC (the desktop
/// shell) doesn't have to carry capture / maintenance tunables it never
/// reads. The defaults are the contract the CLI's auto-ipc relies on:
/// `token_path` must stay derivable from `socket_path` via
/// `nagori_ipc::token_path_for_endpoint`.
#[derive(Debug, Clone)]
pub struct CliIpcConfig {
    /// On Unix this is a filesystem path for the Unix-domain socket. On
    /// Windows it is the named-pipe name (e.g. `\\.\pipe\nagori`) packed in
    /// a `PathBuf` so existing call-sites that store the IPC endpoint keep
    /// working without a platform-conditional type.
    pub socket_path: PathBuf,
    pub token_path: PathBuf,
    /// Maximum time to wait for in-flight IPC handlers to commit during
    /// shutdown before they're aborted. Picked to be longer than the
    /// slowest expected DB write (FTS index update on a large entry) but
    /// short enough that `Ctrl-C` on a stuck daemon still returns quickly.
    /// `run_daemon` reuses this as the drain grace for its background
    /// workers so the daemon has a single shutdown budget.
    pub shutdown_grace: Duration,
    /// Upper bound on concurrent IPC handlers — forwarded into
    /// [`IpcServerConfig`] at startup so the CLI / doctor / regression
    /// tests can tune the in-flight ceiling instead of relying on the
    /// IPC crate's hardcoded default.
    pub max_concurrent_connections: NonZeroUsize,
}

impl Default for CliIpcConfig {
    fn default() -> Self {
        Self {
            socket_path: default_socket_path(),
            token_path: default_token_path_local(),
            shutdown_grace: Duration::from_secs(5),
            max_concurrent_connections: IpcServerConfig::default().max_concurrent_connections,
        }
    }
}

impl CliIpcConfig {
    /// Project the host's tunables onto the [`IpcServerConfig`] surface
    /// the IPC crate consumes. Keeps the accept-loop call sites in
    /// `spawn_ipc_server` to a single line each so the function stays
    /// inside clippy's `too_many_lines` budget.
    const fn ipc_server_config(&self) -> IpcServerConfig {
        IpcServerConfig {
            max_concurrent_connections: self.max_concurrent_connections,
        }
    }
}

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
///
/// We also capture a fingerprint of every runtime file we own (inode for the
/// Unix socket, plain content for the token file) so [`stage_runtime_files`]
/// can refuse to unlink an entry that's been replaced by a fresh daemon
/// running concurrently.
// Not `async`: the bind / token write are synchronous now that the socket
// bind no longer probes peer liveness, and the accept loop is driven by an
// inner `tokio::spawn`. Callers invoke it from within the daemon runtime, so
// the spawn still finds an ambient executor.
fn spawn_ipc_server(
    runtime: NagoriRuntime,
    config: &CliIpcConfig,
    shutdown: ShutdownHandle,
) -> Result<IpcServerTask> {
    let (stop_tx, stop_rx) = watch::channel(false);
    #[cfg(unix)]
    {
        // `run_daemon` holds the data-directory lifetime lock
        // (`nagori_storage::ProcessLock`) for as long as this daemon runs, so
        // no peer daemon owns the same store. A socket inode left behind by a
        // crashed predecessor — or by this daemon's own dead accept loop on a
        // supervisor restart — refuses `connect()` and is reclaimed.
        // `bind_unix_replacing_stale` still refuses a socket with a *live*
        // listener (a daemon sharing this `--ipc` under a different `--db`, or
        // a squatter), so we never unlink a socket someone is serving; removal
        // never hinges on a connect failure alone (the lock is the gate).
        let listener = bind_unix_replacing_stale(&config.socket_path)?;
        let socket_fingerprint = SocketFingerprint::capture(&config.socket_path);
        let (token, listener) = mint_token_unlinking_socket_on_failure(config, listener)?;
        let token_fingerprint = TokenFingerprint::from(&token);
        let grace = config.shutdown_grace;
        let ipc_health = runtime.ipc_health();
        let ipc_config = config.ipc_server_config();
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
                ipc_health,
                ipc_config,
            )
            .await;
            if let Err(err) = result {
                warn!(error = %err, "ipc_server_terminated");
            }
        });
        Ok(IpcServerTask {
            handle,
            stop_tx,
            fingerprints: RuntimeFingerprints {
                socket: socket_fingerprint,
                token: token_fingerprint,
            },
        })
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
        let token_fingerprint = TokenFingerprint::from(&token);
        let grace = config.shutdown_grace;
        let ipc_health = runtime.ipc_health();
        let ipc_config = config.ipc_server_config();
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
                ipc_health,
                ipc_config,
            )
            .await;
            if let Err(err) = result {
                warn!(error = %err, "ipc_server_terminated");
            }
        });
        Ok(IpcServerTask {
            handle,
            stop_tx,
            fingerprints: RuntimeFingerprints {
                socket: SocketFingerprint,
                token: token_fingerprint,
            },
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (runtime, config, shutdown, stop_tx, stop_rx);
        Err(AppError::Unsupported(
            "IPC requires a Unix-like or Windows platform".to_owned(),
        ))
    }
}

/// Mint a fresh auth token and write its file, unlinking the just-bound
/// socket when either step fails.
///
/// The bind has already succeeded at this point, so propagating the error
/// as-is would leave a socket inode with no listener behind. A host that
/// maps the error to a backoff retry (or whose user then disables the
/// toggle) must not strand a dead socket for clients to trip over. The
/// unlink is safe because we just created the inode and still hold the
/// data-directory lock — no peer can own this path. The listener is
/// passed through (and dropped on the failure path before the unlink) so
/// the socket is never removed out from under a live accept loop.
#[cfg(unix)]
fn mint_token_unlinking_socket_on_failure(
    config: &CliIpcConfig,
    listener: tokio::net::UnixListener,
) -> Result<(AuthToken, tokio::net::UnixListener)> {
    let minted = AuthToken::generate()
        .and_then(|token| write_token_file(&config.token_path, &token).map(|()| token));
    match minted {
        Ok(token) => Ok((token, listener)),
        Err(err) => {
            drop(listener);
            if let Err(unlink_err) = std::fs::remove_file(&config.socket_path) {
                warn!(
                    error = %unlink_err,
                    path = %config.socket_path.display(),
                    "socket_cleanup_after_token_failure_failed",
                );
            }
            Err(err)
        }
    }
}

/// Prepare the IPC runtime directories and bind the endpoint.
///
/// Bundling the two means every path that (re)starts the server — daemon
/// startup, the settings-ON arm, and the backoff retry — recreates a
/// missing socket / token directory instead of assuming a one-time setup
/// call already ran. Without this, an initial directory failure would make
/// every retry fail forever: `spawn_ipc_server` alone never recreates the
/// parent directories.
fn start_ipc_server(
    runtime: NagoriRuntime,
    config: &CliIpcConfig,
    shutdown: ShutdownHandle,
) -> Result<IpcServerTask> {
    ensure_ipc_runtime_dirs(config)?;
    spawn_ipc_server(runtime, config, shutdown)
}

/// State the IPC supervisor starts from. [`run_daemon`] always enters with
/// `Running` or `Disabled` (an initial bind failure aborts daemon startup),
/// while [`spawn_cli_ipc_supervisor`] maps an initial failure to
/// `RetryPending` so the host keeps running and the supervisor's existing
/// backoff loop brings IPC up once the cause clears.
enum InitialIpcState {
    /// The endpoint is already bound; supervise it.
    Running(IpcServerTask),
    /// `cli_ipc_enabled` was off at startup; wait for the settings watch.
    Disabled,
    /// The initial bind failed while IPC is enabled; retry with backoff.
    RetryPending,
}

impl InitialIpcState {
    /// Decompose into the supervisor loop's `(server, restart_pending)`
    /// pair. `RetryPending` arms the backoff timer right away: with
    /// settings already enabled, neither the settings arm nor the join
    /// arm would ever fire for a server that never came up, and IPC
    /// would stay dead for good.
    fn into_parts(self) -> (Option<IpcServerTask>, bool) {
        match self {
            Self::Running(server) => (Some(server), false),
            Self::Disabled => (None, false),
            Self::RetryPending => (None, true),
        }
    }
}

struct IpcServerTask {
    handle: tokio::task::JoinHandle<()>,
    stop_tx: watch::Sender<bool>,
    fingerprints: RuntimeFingerprints,
}

impl IpcServerTask {
    fn request_stop(&self) {
        let _ = self.stop_tx.send_replace(true);
    }
}

/// Identifiers captured at create time so [`stage_runtime_files`] can
/// verify the on-disk entry still belongs to *this* daemon before unlinking.
/// Without it a stale shutdown path could race a freshly-launched daemon and
/// remove its socket / token file moments after the new daemon claimed them.
#[derive(Debug, Clone)]
struct RuntimeFingerprints {
    socket: SocketFingerprint,
    token: TokenFingerprint,
}

/// `(dev, ino)` on Unix — the smallest pair that uniquely identifies a
/// filesystem entry across remount. Zero-sized on Windows because the pipe
/// namespace isn't a filesystem and there's nothing to unlink.
#[cfg(unix)]
#[derive(Debug, Clone, Copy)]
struct SocketFingerprint {
    dev: u64,
    ino: u64,
}

#[cfg(unix)]
impl SocketFingerprint {
    fn capture(path: &std::path::Path) -> Self {
        use std::os::unix::fs::MetadataExt;
        match std::fs::metadata(path) {
            Ok(meta) => Self {
                dev: meta.dev(),
                ino: meta.ino(),
            },
            Err(err) => {
                // We just successfully bound the listener — losing the inode
                // right afterwards is exotic enough to warn about, but it
                // shouldn't take the daemon down. Zero/zero won't match any
                // real entry so cleanup will skip rather than mis-delete.
                warn!(error = %err, path = %path.display(), "socket_fingerprint_capture_failed");
                Self { dev: 0, ino: 0 }
            }
        }
    }
}

#[cfg(not(unix))]
#[derive(Debug, Clone, Copy)]
struct SocketFingerprint;

/// The exact bytes we wrote into the token file. Comparing content is
/// portable across Unix/Windows and naturally distinguishes "our file" from
/// "a file another daemon happened to overwrite the same path with" because
/// every launch mints a fresh 32-byte random token.
#[derive(Clone)]
struct TokenFingerprint(String);

// Manual `Debug` so the raw token is never printed via `RuntimeFingerprints`'
// derived `{:?}` (e.g. a supervisor trace or panic). Mirrors `AuthToken` /
// `IpcEnvelope` in `nagori-ipc`.
impl std::fmt::Debug for TokenFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("TokenFingerprint")
            .field(&"[redacted]")
            .finish()
    }
}

impl From<&AuthToken> for TokenFingerprint {
    fn from(token: &AuthToken) -> Self {
        Self(token.as_str().to_owned())
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
    config: CliIpcConfig,
    mut settings_rx: watch::Receiver<nagori_core::AppSettings>,
    mut shutdown: ShutdownHandle,
    initial: InitialIpcState,
) {
    let mut backoff = IPC_RESTART_BACKOFF_INITIAL;
    let (mut server, mut restart_pending) = initial.into_parts();
    loop {
        // The restart timer is only active when we've observed an unexpected
        // accept-loop exit and IPC is still enabled. Encoding it as a future
        // that's `pending()` otherwise lets the select arm coexist with the
        // shutdown / settings / join branches without splitting the loop.
        let restart_timer = async {
            if restart_pending {
                tokio::time::sleep(backoff).await;
            } else {
                std::future::pending::<()>().await;
            }
        };

        // Snapshot before constructing the select arms: `ipc_server_exit`
        // takes `&mut server` for the entire poll, so `server.is_some()`
        // cannot be evaluated inline against any other arm. The boolean
        // is only read by `liveness_tick` to gate its own pending vs.
        // sleep behaviour.
        let have_server = server.is_some();

        tokio::select! {
            // `biased` so a real shutdown beats a coincident accept-loop exit
            // and we don't try to restart a server we're about to tear down.
            biased;
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
                if !enabled {
                    // Settings flipped to disabled. Make sure both the live
                    // server *and* any pending restart get cancelled —
                    // otherwise a respawn that was waiting on the backoff
                    // timer would still fire and resurrect IPC against the
                    // user's preference.
                    if server.is_some() {
                        info!("ipc_disabled_by_settings");
                        if let Some(current) = server.take() {
                            stop_ipc_server(current, &config).await;
                        }
                    } else if restart_pending {
                        // Cancelling a pending restart leaves any runtime
                        // files of the previously-dead server in place: we
                        // no longer hold its fingerprints, and a blind
                        // unlink here would reopen the TOCTOU described at
                        // the join arm below. The files are known-stale
                        // (we hold the data-dir lock) and the next bind —
                        // settings re-enable or a fresh launch — replaces
                        // them atomically.
                        info!("ipc_restart_cancelled_by_settings");
                    }
                    restart_pending = false;
                    backoff = IPC_RESTART_BACKOFF_INITIAL;
                } else if server.is_none() && !restart_pending {
                    // User just turned IPC on while no server was running
                    // and no restart was pending. Start one immediately;
                    // the restart-timer arm will handle the post-failure
                    // backoff path on its own.
                    match start_ipc_server(runtime.clone(), &config, shutdown.clone()) {
                        Ok(next) => {
                            info!(socket = %config.socket_path.display(), "ipc_server_started");
                            server = Some(next);
                            backoff = IPC_RESTART_BACKOFF_INITIAL;
                        }
                        Err(err) => {
                            // A transient failure here (e.g. the runtime dir
                            // not yet writable) must not leave IPC dead for
                            // good. Arm the restart timer so the backoff path
                            // retries; otherwise — with settings already
                            // enabled — neither this arm nor the timer would
                            // ever fire again and IPC would never recover.
                            warn!(error = %err, "ipc_server_start_failed");
                            restart_pending = true;
                        }
                    }
                }
            }
            // Detect the accept loop dying on its own. `stop_ipc_server`
            // takes the handle out of `server` before awaiting it, so this
            // arm only fires for *unexpected* exits.
            //
            // We deliberately do NOT call `cleanup_runtime_files` here even
            // though the accept-loop task (and therefore the listener) is
            // already gone. The next `spawn_ipc_server` below safely
            // replaces both files atomically: `bind_unix_replacing_stale`
            // removes the socket inode our dead listener left behind and
            // rebinds — safe because this process still holds the daemon
            // lifetime lock, so the leftover socket is known-stale, not a
            // peer's; `write_token_file` writes to a sibling temp and renames
            // over the target. Adding a fingerprint-check + rename here would
            // re-introduce a listener-less TOCTOU window — a concurrent fresh
            // daemon (which is no longer blocked at bind because our listener
            // is dead) could write its token between our check and our rename
            // and we'd rename *its* file out from under it. Leaving the
            // stale entries in place until the next spawn is the safer
            // choice.
            join_result = ipc_server_exit(&mut server) => {
                let dead = server.take().expect("ipc_server_exit only fires when server is Some");
                match join_result {
                    Ok(()) => warn!("ipc_server_task_exited_unexpectedly"),
                    Err(err) if err.is_panic() => warn!(error = %err, "ipc_server_task_panicked"),
                    Err(err) => warn!(error = %err, "ipc_server_task_join_failed"),
                }
                drop(dead);
                restart_pending = runtime.current_settings().cli_ipc_enabled;
            }
            // Periodic liveness probe so a wedged accept loop (handler
            // deadlock, kernel-level resource exhaustion) is force-restarted
            // even when the spawned task is still alive. Without this the
            // supervisor only ever respawns on task exit — an accept that
            // simply stops firing would leave IPC silently dead.
            () = liveness_tick(have_server) => {
                if server.is_some()
                    && let Some(age) = wedged_accept_age(&runtime, &config).await
                {
                    warn!(
                        age_ms = u64::try_from(age.as_millis()).unwrap_or(u64::MAX),
                        "ipc_accept_loop_wedged_aborting",
                    );
                    // Abort the task in place; the join_result arm next
                    // iteration reaps it (observes is_cancelled) and the
                    // existing restart flow respawns. Going through the
                    // normal exit path keeps runtime-file fingerprinting
                    // and backoff state machine identical to the
                    // "task exited on its own" case.
                    if let Some(dead) = server.as_ref() {
                        dead.handle.abort();
                    }
                }
            }
            // Fires when `restart_pending` is true after a backoff interval.
            // We use a separate branch (instead of an inline sleep inside the
            // join-result handler) so a respawn failure keeps the timer
            // arm active for the next iteration — without this the loop
            // would bail to the other arms after a single failed retry and
            // never recover.
            () = restart_timer => {
                match start_ipc_server(runtime.clone(), &config, shutdown.clone()) {
                    Ok(next) => {
                        info!(
                            socket = %config.socket_path.display(),
                            backoff_ms = u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX),
                            "ipc_server_restarted_after_unexpected_exit",
                        );
                        server = Some(next);
                        restart_pending = false;
                        backoff = IPC_RESTART_BACKOFF_INITIAL;
                    }
                    Err(err) => {
                        warn!(error = %err, "ipc_server_restart_failed");
                        backoff = backoff.saturating_mul(2).min(IPC_RESTART_BACKOFF_MAX);
                        // restart_pending stays true; we retry after the
                        // new (longer) backoff on the next iteration.
                    }
                }
            }
        }
    }
}

/// Periodic timer that gates the liveness probe. Mirrors the `restart_timer`
/// idiom of returning a `pending()` future when the supervisor has no
/// server to probe — keeps the wedge arm coexisting in the `tokio::select!`
/// without splitting the loop into "have server" / "no server" branches.
async fn liveness_tick(have_server: bool) {
    if have_server {
        tokio::time::sleep(IPC_LIVENESS_PROBE_INTERVAL).await;
    } else {
        std::future::pending::<()>().await;
    }
}

/// Issue a self-probe and report the staleness of
/// `IpcServerHealth::last_accept_at_ms` if it exceeds
/// [`IPC_LIVENESS_WEDGE_THRESHOLD`]. `None` means the loop is healthy
/// (or the probe could not establish a reliable measurement, in which
/// case we conservatively wait for the next tick rather than restart).
async fn wedged_accept_age(runtime: &NagoriRuntime, config: &CliIpcConfig) -> Option<Duration> {
    // Probe success vs failure is informational — the wedge check below
    // measures whether the accept arm bumped the timestamp, which is a
    // stricter test than "the socket file resolves to a listener" (a
    // listener can be present yet its accept future stuck).
    let _ = tokio::time::timeout(IPC_LIVENESS_PROBE_TIMEOUT, probe_ipc_endpoint(config)).await;
    // Give the accept arm a beat to land the timestamp update before we
    // sample it. Without this an immediate sample races the bump and
    // produces false positives on slow machines.
    tokio::time::sleep(IPC_LIVENESS_PROBE_SETTLE).await;
    let now_ms = now_unix_ms();
    let last_ms = runtime.ipc_health().last_accept_at_ms();
    if last_ms == 0 {
        // Pre-seed didn't land yet (vanishingly rare in practice — the
        // accept loop bumps before its first await). Skip this tick.
        return None;
    }
    let age_ms = now_ms.saturating_sub(last_ms);
    if u128::from(age_ms) > IPC_LIVENESS_WEDGE_THRESHOLD.as_millis() {
        Some(Duration::from_millis(age_ms))
    } else {
        None
    }
}

/// Connect to the IPC endpoint as a liveness signal. We drop the stream
/// immediately on success — the per-connection handler enforces its own
/// `FIRST_READ_TIMEOUT`, so a probe that never sends bytes tears down
/// inside ~1s and doesn't park a handler permit.
async fn probe_ipc_endpoint(config: &CliIpcConfig) -> bool {
    #[cfg(unix)]
    {
        tokio::net::UnixStream::connect(&config.socket_path)
            .await
            .is_ok()
    }
    #[cfg(windows)]
    {
        let name = config.socket_path.to_string_lossy().into_owned();
        tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&name)
            .is_ok()
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = config;
        false
    }
}

/// UNIX millis-since-epoch helper mirroring the one in `nagori-ipc`'s
/// `IpcServerHealth`. A pre-1970 clock collapses to `0`, which the wedge
/// check treats as "no measurement yet" so we never flag a restart on
/// the back of a missing baseline.
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Wait for the accept-loop task to exit. When `server` is `None` returns a
/// future that never resolves — this lets us use [`supervise_ipc_server`]'s
/// `tokio::select!` without splitting the loop into "have server" / "no
/// server" branches.
async fn ipc_server_exit(
    server: &mut Option<IpcServerTask>,
) -> std::result::Result<(), tokio::task::JoinError> {
    match server {
        Some(s) => (&mut s.handle).await,
        None => std::future::pending().await,
    }
}

/// Tear down a running IPC server.
///
/// We stage the cleanup *before* signalling the accept loop to stop: while
/// the accept loop is still alive the listener holds the socket inode (Unix)
/// or pipe name (Windows), and any concurrent daemon attempting to claim the
/// same endpoint is blocked at `bind_unix` / `bind_pipe`. That makes the
/// rename-to-private-name step race-free in the common shutdown path —
/// after the rename the public path is unmapped, so the eventual `unlink`
/// can only touch the file we just moved, not a fresh daemon's entry.
async fn stop_ipc_server(server: IpcServerTask, config: &CliIpcConfig) {
    let staged = stage_runtime_files(config, &server.fingerprints);
    server.request_stop();
    drain_one(
        "ipc_serve",
        server.handle,
        config.shutdown_grace + Duration::from_secs(1),
    )
    .await;
    staged.remove();
}

fn spawn_ipc_supervisor(
    runtime: NagoriRuntime,
    config: CliIpcConfig,
    settings_rx: watch::Receiver<nagori_core::AppSettings>,
    shutdown: ShutdownHandle,
    initial: InitialIpcState,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        supervise_ipc_server(runtime, config, settings_rx, shutdown, initial).await;
    })
}

/// Spawn the CLI IPC supervisor against an already-built runtime.
///
/// Used by the desktop shell so it serves the same IPC surface as
/// `nagori daemon run`: same bind path, token handshake, settings-driven
/// ON/OFF, liveness probe, and shutdown cleanup. The caller MUST hold the
/// data-directory `ProcessLock` for the runtime's lifetime — the
/// stale-socket reclaim inside the bind path treats a leftover socket as
/// dead *because* the lock proves no peer owns the store.
///
/// The caller is also responsible for loading persisted settings into the
/// runtime first: `cli_ipc_enabled` is read from `current_settings()`, so
/// serving before the store snapshot lands would honor the compiled-in
/// default instead of the user's choice.
///
/// Unlike [`run_daemon`] — which treats an initial bind failure as a fatal
/// startup error — a failure here only arms the supervisor's backoff
/// retry. A GUI host must keep running when the endpoint is temporarily
/// unavailable (e.g. another process still draining it), and the retry
/// loop brings IPC up once the cause clears.
pub fn spawn_cli_ipc_supervisor(
    runtime: NagoriRuntime,
    config: CliIpcConfig,
    shutdown: ShutdownHandle,
) -> tokio::task::JoinHandle<()> {
    let settings_rx = runtime.settings_subscribe();
    let initial = if runtime.current_settings().cli_ipc_enabled {
        match start_ipc_server(runtime.clone(), &config, shutdown.clone()) {
            Ok(server) => {
                info!(socket = %config.socket_path.display(), "ipc_server_started");
                InitialIpcState::Running(server)
            }
            Err(err) => {
                warn!(error = %err, "ipc_server_start_failed");
                InitialIpcState::RetryPending
            }
        }
    } else {
        info!("ipc_disabled_by_settings");
        InitialIpcState::Disabled
    };
    spawn_ipc_supervisor(runtime, config, settings_rx, shutdown, initial)
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
                        tokio::select! {
                            () = worker_shutdown.cancelled() => return,
                            changed = settings_rx.changed() => {
                                if changed.is_err() {
                                    return;
                                }
                            },
                            () = tokio::time::sleep(interval) => {},
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

fn ensure_ipc_runtime_dirs(config: &CliIpcConfig) -> Result<()> {
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
    // start half-alive rather than retry in the background.
    let initial_ipc_state = if runtime.current_settings().cli_ipc_enabled {
        InitialIpcState::Running(spawn_ipc_server(
            runtime.clone(),
            &config.ipc,
            shutdown.clone(),
        )?)
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

/// Hard cap on the post-abort join: a wedged worker (e.g. blocked in
/// `spawn_blocking` on a syscall that ignores cancellation) would
/// otherwise leave shutdown awaiting `handle.await` forever after
/// `abort()` is called. Two seconds is generous for the cancellation
/// signal to land while still keeping the daemon's exit path bounded.
const POST_ABORT_JOIN_TIMEOUT: Duration = Duration::from_secs(2);

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
            // only log unexpected panics. Bound it so a wedged worker
            // cannot stall shutdown indefinitely.
            match tokio::time::timeout(POST_ABORT_JOIN_TIMEOUT, handle).await {
                Ok(Ok(())) => {}
                Ok(Err(err)) if err.is_cancelled() => {}
                Ok(Err(err)) => warn!(error = %err, worker = name, "drain_abort_join_failed"),
                Err(_) => warn!(worker = name, "worker_drain_timeout"),
            }
        }
    }
}

/// Files moved aside in preparation for cleanup. Holding the `PathBuf`
/// values keeps them ready for a later `remove_file`. The struct is `must_use`
/// so we don't accidentally rename a file aside and then drop the staging
/// info on the floor (which would leave a `.cleanup` orphan behind).
#[must_use = "staged runtime files must be removed or explicitly forgotten"]
struct StagedRuntimeFiles {
    socket: Option<PathBuf>,
    token: Option<PathBuf>,
}

impl StagedRuntimeFiles {
    fn remove(self) {
        if let Some(path) = self.socket
            && let Err(err) = std::fs::remove_file(&path)
        {
            warn!(error = %err, path = %path.display(), "socket_cleanup_failed");
        }
        if let Some(path) = self.token
            && let Err(err) = std::fs::remove_file(&path)
        {
            warn!(error = %err, path = %path.display(), "token_cleanup_failed");
        }
    }
}

/// Rename the socket / token files out from under their public paths into
/// per-daemon staging names *if and only if* they still match the captured
/// fingerprints. Returning a [`StagedRuntimeFiles`] hands the actual unlink
/// to the caller, which can defer it until after the accept loop has
/// drained.
///
/// **Order matters.** We stage the token *first* and the socket *second*.
/// While the socket path is still occupied, any concurrent daemon B is
/// blocked at [`nagori_ipc::bind_unix`] (Unix) / [`nagori_ipc::bind_pipe`]
/// (Windows), which means B has not yet reached `write_token_file` and
/// cannot have planted a fresh token to be rename-stolen by our `rename`.
/// If we staged the socket first the path would free up immediately, B
/// could bind and write its token, and our subsequent token stage would
/// snatch B's freshly written file.
fn stage_runtime_files(
    config: &CliIpcConfig,
    fingerprints: &RuntimeFingerprints,
) -> StagedRuntimeFiles {
    let token = stage_token(&config.token_path, &fingerprints.token);
    let socket = stage_socket(&config.socket_path, &fingerprints.socket);
    StagedRuntimeFiles { socket, token }
}

/// Monotonic counter appended to staging filenames so two near-simultaneous
/// stagings (socket + token in [`stage_runtime_files`], or back-to-back
/// shutdown/respawn cycles within the same `as_nanos()` tick) never produce
/// the same suffix. Without it the previous `pid.nanos` form could collide
/// on hosts whose monotonic clock resolution lags the `SystemTime` granularity
/// — extremely rare in practice, but a collision means `rename(2)` clobbers
/// the earlier staging entry. Combined with the still-included `pid` and
/// `nanos` the resulting suffix is unique within the process and unlikely
/// to collide across daemons.
static STAGING_SUFFIX_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Build a sibling path like `.nagori.sock.<pid>.<nanos>.<seq>.cleanup` for
/// staging. The atomic `<seq>` guarantees no collision between concurrent
/// stagings in the same process; `<pid>.<nanos>` keeps the suffix unique
/// across daemons started in the same monotonic tick.
fn cleanup_staging_path(path: &std::path::Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let name = path.file_name()?.to_string_lossy();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let seq = STAGING_SUFFIX_COUNTER.fetch_add(1, Ordering::Relaxed);
    Some(parent.join(format!(
        ".{name}.{}.{nanos:x}.{seq:x}.cleanup",
        std::process::id(),
    )))
}

#[cfg(unix)]
fn stage_socket(path: &std::path::Path, fingerprint: &SocketFingerprint) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            warn!(error = %err, path = %path.display(), "socket_cleanup_stat_failed");
            return None;
        }
    };
    if meta.dev() != fingerprint.dev || meta.ino() != fingerprint.ino {
        warn!(
            path = %path.display(),
            "socket_cleanup_skipped_fingerprint_mismatch",
        );
        return None;
    }
    let staged = cleanup_staging_path(path)?;
    if let Err(err) = std::fs::rename(path, &staged) {
        warn!(error = %err, path = %path.display(), "socket_cleanup_rename_failed");
        return None;
    }
    Some(staged)
}

#[cfg(not(unix))]
fn stage_socket(_path: &std::path::Path, _fingerprint: &SocketFingerprint) -> Option<PathBuf> {
    // The Windows pipe namespace isn't a filesystem: closing the listener
    // already removes the endpoint. Nothing to stage or unlink.
    None
}

fn stage_token(path: &std::path::Path, fingerprint: &TokenFingerprint) -> Option<PathBuf> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            warn!(error = %err, path = %path.display(), "token_cleanup_read_failed");
            return None;
        }
    };
    if content.trim() != fingerprint.0 {
        warn!(
            path = %path.display(),
            "token_cleanup_skipped_fingerprint_mismatch",
        );
        return None;
    }
    let staged = cleanup_staging_path(path)?;
    if let Err(err) = std::fs::rename(path, &staged) {
        warn!(error = %err, path = %path.display(), "token_cleanup_rename_failed");
        return None;
    }
    Some(staged)
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::{Duration, Instant};

    use nagori_core::AppSettings;
    use nagori_ipc::{IpcClient, IpcRequest, IpcResponse};
    use nagori_storage::SqliteStore;

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

    #[tokio::test]
    async fn settings_change_stops_existing_ipc_server() {
        let temp = tempfile::tempdir().expect("temp dir");
        let config = CliIpcConfig {
            socket_path: temp.path().join("nagori.sock"),
            token_path: temp.path().join("nagori.token"),
            shutdown_grace: Duration::from_millis(50),
            ..CliIpcConfig::default()
        };
        let runtime = NagoriRuntime::builder(SqliteStore::open_memory().expect("memory store"))
            .build_for_test();
        let settings_rx = runtime.settings_subscribe();
        let shutdown = runtime.shutdown_handle();
        let initial_server = spawn_ipc_server(runtime.clone(), &config, shutdown.clone())
            .expect("IPC server should start");
        let supervisor = tokio::spawn(supervise_ipc_server(
            runtime.clone(),
            config.clone(),
            settings_rx,
            shutdown.clone(),
            InitialIpcState::Running(initial_server),
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

    fn test_ipc_config(dir: &std::path::Path) -> CliIpcConfig {
        CliIpcConfig {
            socket_path: dir.join("nagori.sock"),
            token_path: dir.join("nagori.token"),
            shutdown_grace: Duration::from_millis(50),
            ..CliIpcConfig::default()
        }
    }

    fn test_ipc_client(config: &CliIpcConfig) -> IpcClient {
        let token = nagori_ipc::read_token_file(&config.token_path).expect("token file");
        IpcClient::new(config.socket_path.to_string_lossy().into_owned(), token)
            .with_connect_timeout(Duration::from_millis(100))
            .with_request_timeout(Duration::from_secs(1))
    }

    #[tokio::test]
    async fn cli_ipc_supervisor_serves_health_and_cleans_up_on_shutdown() {
        let temp = tempfile::tempdir().expect("temp dir");
        let config = test_ipc_config(temp.path());
        let runtime = NagoriRuntime::builder(SqliteStore::open_memory().expect("memory store"))
            .build_for_test();
        let shutdown = runtime.shutdown_handle();
        let supervisor =
            spawn_cli_ipc_supervisor(runtime.clone(), config.clone(), shutdown.clone());

        let health = test_ipc_client(&config)
            .send(IpcRequest::Health)
            .await
            .expect("health over the desktop-hosted endpoint");
        assert!(matches!(health, IpcResponse::Health(_)));

        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(1), supervisor)
            .await
            .expect("supervisor should stop")
            .expect("supervisor should not panic");
        assert!(
            !config.socket_path.exists(),
            "socket should be removed on shutdown",
        );
        assert!(
            !config.token_path.exists(),
            "token file should be removed on shutdown",
        );
    }

    #[tokio::test]
    async fn cli_ipc_supervisor_initially_disabled_starts_on_settings_enable() {
        let temp = tempfile::tempdir().expect("temp dir");
        let config = test_ipc_config(temp.path());
        let runtime = NagoriRuntime::builder(SqliteStore::open_memory().expect("memory store"))
            .build_for_test();
        runtime
            .save_settings(AppSettings {
                cli_ipc_enabled: false,
                ..AppSettings::default()
            })
            .await
            .expect("disable cli ipc");
        let shutdown = runtime.shutdown_handle();
        let supervisor =
            spawn_cli_ipc_supervisor(runtime.clone(), config.clone(), shutdown.clone());

        // Give the supervisor a beat: a disabled start must not create the
        // socket or leak a token file even transiently.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !config.socket_path.exists(),
            "disabled start must not bind the socket",
        );
        assert!(
            !config.token_path.exists(),
            "disabled start must not write a token file",
        );

        runtime
            .save_settings(AppSettings::default())
            .await
            .expect("enable cli ipc");
        wait_until(Duration::from_secs(2), || config.socket_path.exists())
            .await
            .expect("socket should appear after enabling");
        let health = test_ipc_client(&config)
            .send(IpcRequest::Health)
            .await
            .expect("health after enabling");
        assert!(matches!(health, IpcResponse::Health(_)));

        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(1), supervisor)
            .await
            .expect("supervisor should stop")
            .expect("supervisor should not panic");
    }

    #[tokio::test]
    async fn cli_ipc_supervisor_cleans_socket_when_token_write_fails() {
        let temp = tempfile::tempdir().expect("temp dir");
        let config = test_ipc_config(temp.path());
        // Occupy the token path with a directory: the bind succeeds, but
        // `write_token_file`'s rename over a directory fails, exercising
        // the partial-init path. Without the cleanup a dead socket inode
        // would linger for clients to trip over.
        std::fs::create_dir(&config.token_path).expect("plant blocking dir");
        let runtime = NagoriRuntime::builder(SqliteStore::open_memory().expect("memory store"))
            .build_for_test();
        let shutdown = runtime.shutdown_handle();
        let supervisor =
            spawn_cli_ipc_supervisor(runtime.clone(), config.clone(), shutdown.clone());

        // The failed attempt must not leave the just-bound socket behind.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !config.socket_path.exists(),
            "a token-write failure must unlink the socket it bound",
        );

        // Clearing the cause lets the backoff retry bring IPC up whole.
        std::fs::remove_dir(&config.token_path).expect("remove blocking dir");
        wait_until(Duration::from_secs(3), || config.socket_path.exists())
            .await
            .expect("socket should appear once the retry succeeds");
        let health = test_ipc_client(&config)
            .send(IpcRequest::Health)
            .await
            .expect("health after recovery");
        assert!(matches!(health, IpcResponse::Health(_)));

        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(1), supervisor)
            .await
            .expect("supervisor should stop")
            .expect("supervisor should not panic");
    }

    #[tokio::test]
    async fn cli_ipc_supervisor_recovers_after_initial_bind_failure() {
        let temp = tempfile::tempdir().expect("temp dir");
        // Occupy the runtime directory's path with a plain file so the
        // initial `ensure_ipc_runtime_dirs` (and therefore the bind) fails.
        let blocked_dir = temp.path().join("ipc");
        std::fs::write(&blocked_dir, b"squatter").expect("plant blocking file");
        let config = test_ipc_config(&blocked_dir);
        let runtime = NagoriRuntime::builder(SqliteStore::open_memory().expect("memory store"))
            .build_for_test();
        let shutdown = runtime.shutdown_handle();
        let supervisor =
            spawn_cli_ipc_supervisor(runtime.clone(), config.clone(), shutdown.clone());

        // Clear the cause; the armed backoff retry must bring IPC up
        // without any settings change.
        std::fs::remove_file(&blocked_dir).expect("remove blocking file");
        wait_until(Duration::from_secs(3), || config.socket_path.exists())
            .await
            .expect("socket should appear once the retry succeeds");
        let health = test_ipc_client(&config)
            .send(IpcRequest::Health)
            .await
            .expect("health after recovery");
        assert!(matches!(health, IpcResponse::Health(_)));

        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(1), supervisor)
            .await
            .expect("supervisor should stop")
            .expect("supervisor should not panic");
        assert!(
            !config.socket_path.exists(),
            "socket should be removed on shutdown",
        );
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
