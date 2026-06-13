//! Unix-domain-socket transport: bind, accept loops, and the `serve_unix`
//! entry points. The per-connection envelope handling lives in
//! [`super::connection`]; this module owns the listener lifecycle and the
//! three-stage graceful shutdown.

use std::future::Future;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use nagori_core::{AppError, Result};
use tokio::net::UnixListener;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use super::accept::{
    ACCEPT_RETRY_BACKOFF, acquire_permit_or_shutdown, drain_handlers, is_transient_accept_error,
};
use super::connection::handle_connection;
use super::health::{IpcServerConfig, IpcServerHealth, observe_handler_outcome};
use crate::AuthToken;
use crate::{IpcRequest, IpcResponse};

/// RAII guard for the process umask. Restoring on drop is critical because
/// `umask(2)` is process-global; if we tightened it during `bind` and then
/// panicked, every other file the process creates would inherit the mask.
struct UmaskGuard {
    previous: libc::mode_t,
}

#[allow(unsafe_code)]
impl UmaskGuard {
    fn set(mask: libc::mode_t) -> Self {
        // SAFETY: `umask` is a thread-safe libc call returning the previous
        // mask. There is no failure mode and no aliasing concerns.
        let previous = unsafe { libc::umask(mask) };
        Self { previous }
    }
}

#[allow(unsafe_code)]
impl Drop for UmaskGuard {
    fn drop(&mut self) {
        // SAFETY: see `UmaskGuard::set`.
        let _ = unsafe { libc::umask(self.previous) };
    }
}

/// Bind a `UnixListener` at `path` with `0o600` perms, refusing to touch an
/// entry that already exists.
///
/// This never removes a pre-existing socket. Deciding that a leftover socket
/// is *stale* (its owner is dead) and safe to replace is the caller's job, and
/// is only sound once the caller holds the daemon lifetime lock
/// (`nagori_storage::ProcessLock`) — see [`bind_unix_replacing_stale`]. A
/// single failed `connect()` is deliberately NOT treated as proof of
/// staleness: a transient refusal (listener backlog saturated, peer
/// mid-restart) would otherwise unlink a socket a live daemon still owns and
/// let a second daemon bind the same path, producing two daemons.
pub fn bind_unix(path: impl AsRef<Path>) -> Result<UnixListener> {
    let path = path.as_ref();
    if path.exists() {
        return Err(AppError::Platform(format!(
            "IPC socket is already in use: {}",
            path.display()
        )));
    }
    bind_unix_fresh(path)
}

/// Like [`bind_unix`], but reclaims a *stale* socket inode at `path`.
///
/// **The caller MUST already hold the daemon's data-directory lifetime lock**
/// (`nagori_storage::ProcessLock`) before calling this. That lock proves no
/// other daemon owns the *same store*, so it is the authoritative
/// single-instance gate. Removal is gated on the lock **and** the socket being
/// dead — never on a connect failure *alone* (the failure mode the lifetime
/// lock was introduced to fix): an existing socket is unlinked only when a
/// `connect()` fails with `ECONNREFUSED` (nothing is listening). If a
/// `connect()` *succeeds*, something is actively serving the endpoint — a
/// daemon launched with the same `--ipc` but a *different* `--db` (whose
/// distinct data-dir lock did not exclude it), or a non-nagori squatter — so
/// we refuse rather than unlink a live peer's socket and leave it unreachable.
/// Any *other* connect error leaves liveness undetermined, so we fail closed
/// and refuse to remove. A dead socket inode (a crashed predecessor's, or this
/// daemon's own after a supervisor restart) refuses `connect()` and is
/// reclaimed. The probe is a blocking `std` connect to a local socket, run
/// once on the daemon-startup path.
pub fn bind_unix_replacing_stale(path: impl AsRef<Path>) -> Result<UnixListener> {
    let path = path.as_ref();
    if path.exists() {
        let metadata =
            std::fs::symlink_metadata(path).map_err(|err| AppError::Platform(err.to_string()))?;
        if !metadata.file_type().is_socket() {
            return Err(AppError::Platform(format!(
                "refusing to remove non-socket IPC path: {}",
                path.display()
            )));
        }
        // Only `ECONNREFUSED` reliably means "nobody is listening" (the
        // socket is dead). A *successful* connect means a live peer owns the
        // endpoint — refuse. Any *other* error (`EACCES`, `EMFILE`, `EINTR`,
        // …) leaves liveness undetermined, so we fail closed and refuse to
        // remove rather than risk unlinking a live socket on an inconclusive
        // probe.
        match std::os::unix::net::UnixStream::connect(path) {
            Ok(_) => {
                return Err(AppError::Platform(format!(
                    "IPC socket is already in use: {}",
                    path.display()
                )));
            }
            Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused => {
                // Dead socket — fall through to reclaim it.
            }
            Err(err) => {
                return Err(AppError::Platform(format!(
                    "refusing to replace IPC socket {}: liveness probe was inconclusive ({err})",
                    path.display()
                )));
            }
        }
        std::fs::remove_file(path).map_err(|err| AppError::Platform(err.to_string()))?;
    }
    bind_unix_fresh(path)
}

/// Bind a fresh listener at `path` (which must not already exist) with a
/// `0o600` socket inode. Shared by [`bind_unix`] and
/// [`bind_unix_replacing_stale`].
fn bind_unix_fresh(path: &Path) -> Result<UnixListener> {
    // `bind` creates the socket inode using the process umask. Tighten the
    // mask to 0o077 around the call so the file is born `0o600` and there
    // is no window where a co-tenant on the same machine could `connect()`
    // before the explicit chmod below.
    let listener = {
        let _restore = UmaskGuard::set(0o077);
        UnixListener::bind(path).map_err(|err| AppError::Platform(err.to_string()))?
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|err| AppError::Platform(err.to_string()))?;
    Ok(listener)
}

/// Accept connections on `listener` and dispatch authenticated requests to `handler`.
///
/// Validates the per-launch auth token in each `IpcEnvelope` before
/// dispatching the inner `IpcRequest`. Loops until the listener errors. Token
/// validation runs in constant time (see `AuthToken::verify`) so the response
/// time can't be used to brute the token byte-by-byte.
///
/// This entry point never returns under normal operation. Callers that need
/// to drive a clean shutdown (drop the listener, then drain in-flight
/// handlers, then abort) should use [`accept_loop_with_shutdown`] instead.
pub async fn accept_loop<F, Fut>(
    listener: UnixListener,
    expected_token: AuthToken,
    handler: F,
) -> Result<()>
where
    F: Fn(IpcRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = IpcResponse> + Send + 'static,
{
    accept_loop_with_shutdown(
        listener,
        expected_token,
        handler,
        std::future::pending::<()>(),
        Duration::from_secs(0),
        IpcServerHealth::default(),
        IpcServerConfig::default(),
    )
    .await
}

/// Three-stage graceful shutdown variant of [`accept_loop`].
///
/// 1. While `shutdown` is pending, accept connections normally and spawn
///    each handler into a `JoinSet` so we can address them collectively.
/// 2. When `shutdown` fires, the listener is dropped (no new connections)
///    and we wait up to `drain_grace` for the spawned handlers to finish.
///    In-flight transactions get a chance to commit instead of being
///    half-applied.
/// 3. Anything still running after `drain_grace` is aborted; any abort
///    panics are observed via `JoinSet::join_next` so they don't leak.
pub async fn accept_loop_with_shutdown<F, Fut, S>(
    listener: UnixListener,
    expected_token: AuthToken,
    handler: F,
    shutdown: S,
    drain_grace: Duration,
    server_health: IpcServerHealth,
    config: IpcServerConfig,
) -> Result<()>
where
    F: Fn(IpcRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = IpcResponse> + Send + 'static,
    S: Future<Output = ()> + Send,
{
    let handler = Arc::new(handler);
    let token = Arc::new(expected_token);
    let semaphore = Arc::new(Semaphore::new(config.max_concurrent_connections.get()));
    let mut tasks: JoinSet<()> = JoinSet::new();
    // Stamp the active tuning onto the health snapshot so `nagori
    // doctor` / `nagori health` can show the active connection ceiling
    // without an extra IPC roundtrip. Done before the first accept so a
    // probe that lands during startup never observes a `0` placeholder.
    server_health.record_config(config);
    // Seed the liveness clock so an idle daemon doesn't look wedged.
    // Without this the supervisor would observe a `0` timestamp on its
    // first probe and immediately escalate to restart — record once
    // before we begin accepting so the wedge check measures elapsed
    // time relative to the loop becoming ready, not the UNIX epoch.
    server_health.record_accept();

    tokio::pin!(shutdown);
    let accept_result = loop {
        tokio::select! {
            biased;
            () = &mut shutdown => break Ok(()),
            accept = listener.accept() => {
                let (stream, _) = match accept {
                    Ok(accepted) => accepted,
                    // `EMFILE`/`ENFILE`, `ECONNABORTED`, … resolve on their
                    // own; breaking here would tear down the whole IPC
                    // surface (and pay a supervisor respawn + re-bind) over
                    // a single hiccup. Back off briefly — raced against
                    // shutdown so a draining daemon never waits it out —
                    // and keep accepting.
                    Err(err) if is_transient_accept_error(&err) => {
                        tracing::warn!(error = %err, "ipc_accept_transient_error");
                        tokio::select! {
                            biased;
                            () = &mut shutdown => break Ok(()),
                            () = tokio::time::sleep(ACCEPT_RETRY_BACKOFF) => {}
                        }
                        continue;
                    }
                    Err(err) => break Err(AppError::Platform(err.to_string())),
                };
                // Bump the liveness timestamp before we touch the
                // semaphore. The supervisor's wedge probe relies on this
                // landing per accept; running it before the permit await
                // means even a saturated handler pool keeps the timestamp
                // advancing as long as accept() itself is still firing.
                server_health.record_accept();
                // Race permit acquisition against shutdown (see
                // `acquire_permit_or_shutdown`); on shutdown, refuse the
                // just-accepted connection by dropping its stream — the
                // client sees EOF and we proceed to the drain stage.
                let permit = match acquire_permit_or_shutdown(
                    shutdown.as_mut(),
                    semaphore.clone(),
                )
                .await
                {
                    Ok(Some(permit)) => permit,
                    Ok(None) => {
                        drop(stream);
                        break Ok(());
                    }
                    Err(err) => break Err(err),
                };
                let handler = handler.clone();
                let token = token.clone();
                tasks.spawn(handle_connection(stream, permit, handler, token));
            }
            // Reap completed handlers so the `JoinSet` doesn't grow without
            // bound for the lifetime of the daemon. Route the result through
            // `observe_handler_outcome` so a panicking handler is logged and
            // counted in `IpcServerHealth` instead of being silently dropped.
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                observe_handler_outcome(&server_health, result);
            }
        }
    };

    // Stage 1: drop the listener so no further `accept()` succeeds even
    // for clients that beat the shutdown signal in. Stages 2 and 3 (bounded
    // drain, then abort-and-reap) are shared with the named-pipe loop.
    drop(listener);
    drain_handlers(tasks, drain_grace, &server_health).await;

    accept_result
}

pub async fn serve_unix<F, Fut>(path: impl AsRef<Path>, token: AuthToken, handler: F) -> Result<()>
where
    F: Fn(IpcRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = IpcResponse> + Send + 'static,
{
    let listener = bind_unix(path)?;
    accept_loop(listener, token, handler).await
}

/// `serve_unix` variant that threads through a caller-supplied
/// `IpcServerHealth` so per-connection handler panics are counted and
/// surfaced via `nagori health` / `nagori doctor`.
pub async fn serve_unix_with_health<F, Fut>(
    path: impl AsRef<Path>,
    token: AuthToken,
    handler: F,
    server_health: IpcServerHealth,
    config: IpcServerConfig,
) -> Result<()>
where
    F: Fn(IpcRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = IpcResponse> + Send + 'static,
{
    let listener = bind_unix(path)?;
    accept_loop_with_shutdown(
        listener,
        token,
        handler,
        std::future::pending::<()>(),
        Duration::from_secs(0),
        server_health,
        config,
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use nagori_core::AppError;

    use crate::{
        AddEntryRequest, EntryDto, GetEntryRequest, HealthResponse, IpcClient, ListRecentRequest,
        SearchRequest, SearchResponse,
    };

    use super::*;

    fn test_token() -> AuthToken {
        AuthToken::generate().expect("token should generate")
    }

    #[tokio::test]
    async fn refuses_to_unlink_active_socket() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let path = dir.path().join("nagori.sock");
        let _listener =
            tokio::net::UnixListener::bind(&path).expect("test listener should bind socket");

        let err = serve_unix(&path, test_token(), |_request| async { IpcResponse::Ack })
            .await
            .expect_err("active socket should be refused");

        assert!(matches!(err, AppError::Platform(message) if message.contains("already in use")));
        assert!(path.exists());
    }

    #[test]
    fn bind_unix_refuses_a_pre_existing_socket_even_when_dead() {
        // `bind_unix` is the conservative primitive: it must never unlink an
        // entry it finds, not even a *dead* socket (no listener). Removing a
        // stale socket is the lifetime-lock holder's job via
        // `bind_unix_replacing_stale`; inferring staleness here from the
        // absence of a peer is exactly the fragile heuristic we removed.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nagori.sock");
        // Bind then drop to leave a socket inode with nobody listening.
        drop(std::os::unix::net::UnixListener::bind(&path).expect("seed dead socket"));
        assert!(path.exists());

        let err = bind_unix(&path).expect_err("a pre-existing socket must be refused");
        assert!(matches!(err, AppError::Platform(message) if message.contains("already in use")));
        // The inode is left untouched — we did not unlink it.
        assert!(path.exists());
    }

    #[tokio::test]
    async fn bind_unix_replacing_stale_reclaims_a_dead_socket() {
        // The lock-gated path: a socket inode left by a crashed predecessor is
        // known-stale (the caller holds the lifetime lock), so it is removed
        // and rebinding succeeds. This backs both the daemon's first bind and
        // its supervisor restart over its own dead listener. Runs under a
        // Tokio runtime because `UnixListener::bind` registers with the
        // reactor.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nagori.sock");
        drop(std::os::unix::net::UnixListener::bind(&path).expect("seed dead socket"));
        assert!(path.exists());

        let listener =
            bind_unix_replacing_stale(&path).expect("a dead socket should be reclaimable");
        // The fresh listener owns a new socket inode at the same path.
        assert!(path.exists());
        drop(listener);
    }

    #[tokio::test]
    async fn bind_unix_replacing_stale_refuses_a_live_socket() {
        // Guards the cross-config case the data-dir lock cannot: a daemon
        // launched with the same `--ipc` but a *different* `--db` holds a
        // distinct data-dir lock, so it is not excluded — but it IS actively
        // listening. We must refuse to unlink a socket someone is serving
        // (which would leave them unreachable), not reclaim it. A bound
        // listener answers `connect()` from its backlog even without an
        // explicit `accept()`.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nagori.sock");
        let _live =
            tokio::net::UnixListener::bind(&path).expect("seed a live listener on the path");

        let err = bind_unix_replacing_stale(&path)
            .expect_err("a socket with a live listener must be refused, not reclaimed");
        assert!(matches!(err, AppError::Platform(message) if message.contains("already in use")));
        assert!(path.exists());
    }

    #[test]
    fn bind_unix_replacing_stale_refuses_a_non_socket_path() {
        // Even holding the lifetime lock, we never clobber a non-socket entry
        // squatting the IPC path — it is not ours to delete.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nagori.sock");
        std::fs::write(&path, b"not a socket").expect("seed regular file");

        let err =
            bind_unix_replacing_stale(&path).expect_err("a non-socket entry must not be removed");
        assert!(matches!(err, AppError::Platform(message) if message.contains("non-socket")));
        assert!(path.exists());
    }

    /// Boot a `serve_unix` server backed by a closure handler in the
    /// background and tear it down once the test scope exits. The returned
    /// `JoinHandle` is aborted in `Drop`-equivalent fashion at the call
    /// site by overwriting the variable; the listening task either yields
    /// on the next `accept` or finishes when the temp dir is removed.
    async fn spawn_handler<F, Fut>(
        path: std::path::PathBuf,
        token: AuthToken,
        handler: F,
    ) -> tokio::task::JoinHandle<()>
    where
        F: Fn(IpcRequest) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = IpcResponse> + Send + 'static,
    {
        let server_path = path.clone();
        let task = tokio::spawn(async move {
            // Errors from `serve_unix` (e.g. abort) are expected when the
            // test concludes; we ignore them.
            let _ = serve_unix(&server_path, token, handler).await;
        });
        // Wait for the socket file to appear before returning so callers
        // can connect immediately. Bounded retry — fail loudly if bind
        // never succeeds.
        for _ in 0..50 {
            if path.exists() {
                return task;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("ipc socket never appeared at {}", path.display());
    }

    #[tokio::test]
    async fn round_trip_health_request_returns_health_response() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("health.sock");
        let token = test_token();
        let server = spawn_handler(path.clone(), token.clone(), |request| async move {
            assert!(matches!(request, IpcRequest::Health));
            IpcResponse::Health(HealthResponse {
                ok: true,
                version: "test-version".to_owned(),
                maintenance: crate::MaintenanceHealthReport::default(),
                capture: crate::CaptureHealthReport::default(),
                ipc: crate::IpcHealthReport::default(),
            })
        })
        .await;

        let client = IpcClient::new(path.to_string_lossy().to_string(), token);
        let response = client
            .send(IpcRequest::Health)
            .await
            .expect("health round-trip");
        let IpcResponse::Health(health) = response else {
            panic!("expected health response, got {response:?}");
        };
        assert!(health.ok);
        assert_eq!(health.version, "test-version");
        server.abort();
    }

    #[tokio::test]
    async fn round_trip_search_request_passes_query_and_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("search.sock");
        let token = test_token();
        let server = spawn_handler(path.clone(), token.clone(), |request| async move {
            let IpcRequest::Search(SearchRequest { query, limit }) = request else {
                return IpcResponse::Error(crate::IpcError {
                    code: "test_failure".to_owned(),
                    message: "unexpected request kind".to_owned(),
                    recoverable: false,
                });
            };
            assert_eq!(query, "needle");
            assert_eq!(limit, 7);
            IpcResponse::Search(SearchResponse {
                results: Vec::new(),
            })
        })
        .await;

        let client = IpcClient::new(path.to_string_lossy().to_string(), token);
        let response = client
            .send(IpcRequest::Search(SearchRequest {
                query: "needle".to_owned(),
                limit: 7,
            }))
            .await
            .expect("search round-trip");
        assert!(
            matches!(response, IpcResponse::Search(SearchResponse { results }) if results.is_empty())
        );
        server.abort();
    }

    #[tokio::test]
    async fn rejects_request_with_wrong_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth.sock");
        let server = spawn_handler(path.clone(), test_token(), |_request| async {
            // Should never run for an unauthorized request — assert below
            // verifies the response was synthesised by the server before
            // dispatch.
            IpcResponse::Ack
        })
        .await;

        let bogus = AuthToken::generate().expect("alt token");
        let client = IpcClient::new(path.to_string_lossy().to_string(), bogus);
        let response = client
            .send(IpcRequest::Health)
            .await
            .expect("auth round-trip");
        let IpcResponse::Error(err) = response else {
            panic!("expected error response, got {response:?}");
        };
        assert_eq!(err.code, "unauthorized");
        assert!(!err.recoverable);
        server.abort();
    }

    #[tokio::test]
    async fn round_trip_invalid_request_returns_error_response() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bad.sock");
        // The handler is unreachable for malformed payloads — the server
        // synthesises an invalid_request error before the closure runs.
        let server = spawn_handler(path.clone(), test_token(), |_request| async {
            IpcResponse::Ack
        })
        .await;

        let mut stream = tokio::net::UnixStream::connect(&path)
            .await
            .expect("connect");
        tokio::io::AsyncWriteExt::write_all(&mut stream, b"not-json-payload\n")
            .await
            .expect("write");
        let mut buf = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let n = tokio::io::AsyncReadExt::read(&mut stream, &mut chunk)
                .await
                .expect("read");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.contains(&b'\n') {
                break;
            }
        }
        let line = buf
            .split(|byte| *byte == b'\n')
            .next()
            .expect("response line");
        let response: IpcResponse = serde_json::from_slice(line).expect("decode response");
        let IpcResponse::Error(err) = response else {
            panic!("expected error response, got {response:?}");
        };
        assert_eq!(err.code, "invalid_request");
        assert!(err.recoverable);
        server.abort();
    }

    #[tokio::test]
    async fn round_trip_handler_serialises_entry_payloads_intact() {
        use time::OffsetDateTime;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("entry.sock");
        let token = test_token();
        let server = spawn_handler(path.clone(), token.clone(), move |request| async move {
            match request {
                IpcRequest::AddEntry(AddEntryRequest { text }) => {
                    assert_eq!(text, "ipc payload");
                    IpcResponse::Entry(EntryDto {
                        id: nagori_core::EntryId::new(),
                        kind: nagori_core::ContentKind::Text,
                        text: Some(text),
                        preview: "ipc payload".to_owned(),
                        created_at: OffsetDateTime::now_utc(),
                        updated_at: OffsetDateTime::now_utc(),
                        last_used_at: None,
                        use_count: 0,
                        pinned: false,
                        source_app_name: None,
                        sensitivity: nagori_core::Sensitivity::Public,
                        representation_summary: Vec::new(),
                    })
                }
                IpcRequest::ListRecent(ListRecentRequest { .. })
                | IpcRequest::GetEntry(GetEntryRequest { .. }) => IpcResponse::Entries(Vec::new()),
                _ => IpcResponse::Ack,
            }
        })
        .await;

        let client = IpcClient::new(path.to_string_lossy().to_string(), token);
        let response = client
            .send(IpcRequest::AddEntry(AddEntryRequest {
                text: "ipc payload".to_owned(),
            }))
            .await
            .expect("add round-trip");
        let IpcResponse::Entry(entry) = response else {
            panic!("expected entry response, got {response:?}");
        };
        assert_eq!(entry.text.as_deref(), Some("ipc payload"));
        assert_eq!(entry.preview, "ipc payload");
        server.abort();
    }

    #[tokio::test]
    async fn shutdown_drains_in_flight_handler_within_grace() {
        // The graceful-shutdown contract: a request that started before
        // shutdown was signalled gets to finish (and reach the client),
        // provided it can complete within the grace period. Without this
        // we'd leave half-applied DB transactions when the user hits
        // Ctrl-C mid-request.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("drain.sock");
        let token = test_token();
        let listener = bind_unix(&path).expect("bind");
        let shutdown = Arc::new(tokio::sync::Notify::new());
        let shutdown_for_server = shutdown.clone();
        let server_token = token.clone();
        let server = tokio::spawn(async move {
            accept_loop_with_shutdown(
                listener,
                server_token,
                |_request| async move {
                    // Outlast the `notify_waiters` below but stay well
                    // inside the grace window so the drain can observe
                    // the response landing.
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    IpcResponse::Ack
                },
                async move { shutdown_for_server.notified().await },
                Duration::from_secs(2),
                IpcServerHealth::default(),
                IpcServerConfig::default(),
            )
            .await
        });

        // Kick off a request as its own task so the connect + write
        // actually run before we signal shutdown — `client.send` is a
        // lazy future, so awaiting it after `notify_waiters` would race
        // the listener drop.
        let client = IpcClient::new(path.to_string_lossy().into_owned(), token);
        let request = tokio::spawn(async move { client.send(IpcRequest::Health).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        shutdown.notify_waiters();

        let response = tokio::time::timeout(Duration::from_secs(3), request)
            .await
            .expect("response should arrive within grace + slack")
            .expect("request task should not panic")
            .expect("client send should succeed");
        assert!(matches!(response, IpcResponse::Ack));

        let outcome = tokio::time::timeout(Duration::from_secs(3), server)
            .await
            .expect("server should finish within grace + slack")
            .expect("server task should not panic");
        assert!(
            outcome.is_ok(),
            "accept loop should exit cleanly: {outcome:?}"
        );
    }

    #[tokio::test]
    async fn shutdown_observed_promptly_when_permit_pool_is_saturated() {
        // Regression: with all 32 handler permits taken and the 33rd
        // connection blocked on `acquire_owned().await`, the accept
        // loop must still observe `shutdown` and drop the listener —
        // shutdown latency must be independent of handler progress.
        // Before the fix, the loop was stuck on the inner permit
        // acquisition and would not poll shutdown until a handler
        // freed a permit (i.e. `drain_grace` aborted them).
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("saturated.sock");
        let token = test_token();
        let listener = bind_unix(&path).expect("bind");

        let release = Arc::new(tokio::sync::Notify::new());
        let started = Arc::new(AtomicUsize::new(0));
        let shutdown = Arc::new(tokio::sync::Notify::new());

        let release_for_server = release.clone();
        let started_for_server = started.clone();
        let shutdown_for_server = shutdown.clone();
        let server_token = token.clone();
        // Pick a drain_grace that is loosely bounded but large enough
        // that "shutdown observed within 500 ms" is a meaningful
        // assertion: a regression would push the listener-drop out
        // to drain_grace == 5 s.
        let server = tokio::spawn(async move {
            accept_loop_with_shutdown(
                listener,
                server_token,
                move |_request| {
                    let release = release_for_server.clone();
                    let started = started_for_server.clone();
                    async move {
                        started.fetch_add(1, Ordering::SeqCst);
                        release.notified().await;
                        IpcResponse::Ack
                    }
                },
                async move { shutdown_for_server.notified().await },
                Duration::from_secs(5),
                IpcServerHealth::default(),
                IpcServerConfig::default(),
            )
            .await
        });

        // Saturate the 32-handler pool. We spawn each request as its
        // own task so the connect + write actually run; the handlers
        // then park on `release.notified()`.
        let mut clients = Vec::with_capacity(32);
        for _ in 0..32 {
            let client_path = path.clone();
            let client_token = token.clone();
            clients.push(tokio::spawn(async move {
                let client =
                    IpcClient::new(client_path.to_string_lossy().to_string(), client_token);
                client.send(IpcRequest::Health).await
            }));
        }
        // Wait until all 32 handlers have started; bound the wait so a
        // hung server doesn't hang the test.
        for _ in 0..200 {
            if started.load(Ordering::SeqCst) >= 32 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            started.load(Ordering::SeqCst),
            32,
            "all 32 handlers should be in flight before we issue the 33rd connection"
        );

        // Issue the 33rd connection. Its accept will succeed but the
        // server's permit acquisition will block until shutdown wins.
        let blocked_path = path.clone();
        let blocked_token = token.clone();
        let blocked = tokio::spawn(async move {
            let client = IpcClient::new(blocked_path.to_string_lossy().to_string(), blocked_token);
            client.send(IpcRequest::Health).await
        });
        // Give the server time to accept the 33rd and reach the
        // permit-acquisition select arm.
        tokio::time::sleep(Duration::from_millis(100)).await;

        let shutdown_at = std::time::Instant::now();
        shutdown.notify_waiters();

        // After shutdown the listener should be dropped quickly. We
        // probe by attempting fresh connects; once the file is gone
        // (`bind_unix` removes it before binding, but for shutdown we
        // just drop the listener — so the inode lingers and connects
        // get ECONNREFUSED) we know the server has reached at least
        // stage 1 of the drain.
        let mut listener_gone = false;
        for _ in 0..50 {
            if tokio::net::UnixStream::connect(&path).await.is_err() {
                listener_gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let elapsed = shutdown_at.elapsed();
        assert!(
            listener_gone,
            "listener should refuse new connections after shutdown",
        );
        assert!(
            elapsed < Duration::from_millis(500),
            "shutdown should be observed within 500 ms even with saturated permits, took {elapsed:?}",
        );

        // Release the parked handlers so the drain stage can complete
        // without paying drain_grace.
        release.notify_waiters();

        let outcome = tokio::time::timeout(Duration::from_secs(7), server)
            .await
            .expect("server should finish after release")
            .expect("server task should not panic");
        assert!(
            outcome.is_ok(),
            "accept loop should exit cleanly: {outcome:?}",
        );
        for client in clients {
            let _ = client.await;
        }
        let _ = blocked.await;
    }

    #[tokio::test]
    async fn handler_panic_increments_ipc_server_health() {
        // Regression: before the fix, a panic inside a per-connection
        // handler was silently dropped by `JoinSet::join_next()` — no
        // log line, no health counter. Verify the panic now lands in
        // `IpcServerHealth` and is logged so dashboards can see it.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("panic.sock");
        let token = test_token();
        let listener = bind_unix(&path).expect("bind");
        let shutdown = Arc::new(tokio::sync::Notify::new());
        let shutdown_for_server = shutdown.clone();
        let server_token = token.clone();
        let health = IpcServerHealth::new();
        let server_health = health.clone();
        let server = tokio::spawn(async move {
            accept_loop_with_shutdown(
                listener,
                server_token,
                |_request| async move {
                    panic!("induced panic");
                },
                async move { shutdown_for_server.notified().await },
                Duration::from_secs(1),
                server_health,
                IpcServerConfig::default(),
            )
            .await
        });

        // Drive a request so the handler runs and panics.
        let client_path = path.clone();
        let client_token = token.clone();
        let request = tokio::spawn(async move {
            let client = IpcClient::new(client_path.to_string_lossy().to_string(), client_token);
            client.send(IpcRequest::Health).await
        });
        // Client side will see EOF (the handler panic drops the
        // stream); just await without asserting the response shape.
        let _ = tokio::time::timeout(Duration::from_secs(1), request).await;

        // Give the JoinSet a beat to reap the panicked task.
        for _ in 0..50 {
            if health.handler_panic_count() >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            health.handler_panic_count() >= 1,
            "handler panic should be reflected in IpcServerHealth"
        );
        assert!(
            health.last_panic_message().is_some(),
            "last_panic_message should be populated after a panic"
        );

        shutdown.notify_waiters();
        let _ = tokio::time::timeout(Duration::from_secs(2), server).await;
    }

    #[tokio::test]
    async fn oversized_handler_response_is_replaced_with_a_structured_error() {
        // A handler whose serialized response exceeds the wire cap must not be
        // sent verbatim (it would blow the client's bounded reader and look
        // like a truncated half-JSON). The server replaces it with a small
        // `response_too_large` envelope the caller can act on.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("toobig.sock");
        let token = test_token();
        let server = spawn_handler(path.clone(), token.clone(), |_request| async {
            IpcResponse::Error(crate::IpcError {
                code: "huge".to_owned(),
                // The message alone overruns the 1 MiB response ceiling.
                message: "x".repeat(crate::MAX_IPC_BYTES + 1024),
                recoverable: false,
            })
        })
        .await;

        let client = IpcClient::new(path.to_string_lossy().to_string(), token);
        let response = client
            .send(IpcRequest::Health)
            .await
            .expect("the small replacement envelope round-trips");
        let IpcResponse::Error(err) = response else {
            panic!("expected an error response, got {response:?}");
        };
        assert_eq!(err.code, "response_too_large");
        assert!(!err.recoverable);
        server.abort();
    }

    #[tokio::test]
    async fn server_rejects_an_oversized_request_line() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Writing raw bytes bypasses the client's pre-send size check, so this
        // exercises the *server's* request-size ceiling end to end. The handler
        // must never run — `read_bounded_line` rejects the line before parse.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bigreq.sock");
        let server = spawn_handler(path.clone(), test_token(), |_request| async {
            IpcResponse::Ack
        })
        .await;

        let stream = tokio::net::UnixStream::connect(&path)
            .await
            .expect("connect");
        let (mut read_half, mut write_half) = stream.into_split();

        // Push the oversized line from a separate task: once the server hits
        // the ceiling it stops reading and closes, so the tail write fails —
        // splitting lets us read the rejection concurrently rather than
        // deadlocking on a half-flushed write.
        let writer = tokio::spawn(async move {
            let oversized = vec![b'a'; crate::MAX_IPC_BYTES + 64];
            let _ = write_half.write_all(&oversized).await;
            let _ = write_half.write_all(b"\n").await;
            let _ = write_half.flush().await;
        });

        let mut buf = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = read_half.read(&mut chunk).await.expect("read response");
            if read == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..read]);
            if buf.contains(&b'\n') {
                break;
            }
        }
        writer.abort();

        let line = buf
            .split(|&b| b == b'\n')
            .next()
            .expect("a response line before EOF");
        let response: IpcResponse = serde_json::from_slice(line).expect("parse response");
        let IpcResponse::Error(err) = response else {
            panic!("expected an error response, got {response:?}");
        };
        assert_eq!(err.code, "invalid_request");
        assert!(err.message.contains("too large"), "got {}", err.message);
        server.abort();
    }
}
