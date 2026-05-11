use std::{future::Future, path::Path};

use nagori_core::{AppError, Result};
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
#[cfg(any(unix, windows))]
use std::sync::Arc;
#[cfg(any(unix, windows))]
use std::time::Duration;
#[cfg(unix)]
use tokio::net::UnixListener;
#[cfg(any(unix, windows))]
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::Semaphore,
    task::JoinSet,
    time::timeout,
};
#[cfg(any(unix, windows))]
use tracing::warn;

#[cfg(any(unix, windows))]
use crate::AuthToken;
use crate::{IpcEnvelope, IpcRequest, IpcResponse};

#[cfg(any(unix, windows))]
const MAX_IPC_REQUEST_BYTES: usize = crate::MAX_IPC_BYTES;

/// Cap each daemon -> client response at the same byte budget the client
/// already enforces (`crate::MAX_IPC_BYTES`). The check runs *after*
/// `serde_json::to_vec`, so the handler still pays for constructing and
/// serialising the response — bounding peak daemon RSS for pathological
/// requests (e.g. `ListRecent` with `limit = usize::MAX`) requires
/// request-level limits at each handler. What this guard does buy is:
/// (a) we never write a line the client's bounded reader would reject as a
/// truncated half-JSON, and (b) we drop the oversized payload immediately
/// in favour of a small structured rejection so the connection can be
/// reused instead of stalling until timeout.
#[cfg(any(unix, windows))]
const MAX_IPC_RESPONSE_BYTES: usize = crate::MAX_IPC_BYTES;

/// Hard ceiling on how long a single connection can block before the
/// envelope is fully read. CLI clients send one short JSON line and
/// disconnect, so a few seconds is plenty of slack for the slowest
/// realistic local round-trip. Kept tight because on Windows the named
/// pipe uses the default DACL — any local user can open a connection
/// and would otherwise park one of the 32 permits for the full window
/// without ever sending a byte, starving the legitimate CLI.
#[cfg(any(unix, windows))]
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Sub-budget for the first read. If the peer hasn't sent any bytes
/// within this window we drop the connection immediately. Caps the
/// silent-peer slow-loris cost at roughly one second per parked permit,
/// while still letting a slightly stalled writer (e.g. the CLI flushing
/// stdin) complete the envelope under `READ_TIMEOUT`.
#[cfg(any(unix, windows))]
const FIRST_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// RAII guard for the process umask. Restoring on drop is critical because
/// `umask(2)` is process-global; if we tightened it during `bind` and then
/// panicked, every other file the process creates would inherit the mask.
#[cfg(unix)]
struct UmaskGuard {
    previous: libc::mode_t,
}

#[cfg(unix)]
#[allow(unsafe_code)]
impl UmaskGuard {
    fn set(mask: libc::mode_t) -> Self {
        // SAFETY: `umask` is a thread-safe libc call returning the previous
        // mask. There is no failure mode and no aliasing concerns.
        let previous = unsafe { libc::umask(mask) };
        Self { previous }
    }
}

#[cfg(unix)]
#[allow(unsafe_code)]
impl Drop for UmaskGuard {
    fn drop(&mut self) {
        // SAFETY: see `UmaskGuard::set`.
        let _ = unsafe { libc::umask(self.previous) };
    }
}

/// Bind a `UnixListener` at `path` with `0o600` perms.
///
/// Synchronous-friendly callers (daemon startup) can `await` this and
/// propagate the failure before signalling that they are ready, which is what
/// the daemon needs to fail fast on bind errors instead of staying
/// half-alive.
#[cfg(unix)]
pub async fn bind_unix(path: impl AsRef<Path>) -> Result<UnixListener> {
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
        if tokio::net::UnixStream::connect(path).await.is_ok() {
            return Err(AppError::Platform(format!(
                "IPC socket is already in use: {}",
                path.display()
            )));
        }
        std::fs::remove_file(path).map_err(|err| AppError::Platform(err.to_string()))?;
    }
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
#[cfg(unix)]
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
#[cfg(unix)]
pub async fn accept_loop_with_shutdown<F, Fut, S>(
    listener: UnixListener,
    expected_token: AuthToken,
    handler: F,
    shutdown: S,
    drain_grace: Duration,
) -> Result<()>
where
    F: Fn(IpcRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = IpcResponse> + Send + 'static,
    S: Future<Output = ()> + Send,
{
    let handler = Arc::new(handler);
    let token = Arc::new(expected_token);
    let semaphore = Arc::new(Semaphore::new(32));
    let mut tasks: JoinSet<()> = JoinSet::new();

    tokio::pin!(shutdown);
    let accept_result = loop {
        tokio::select! {
            biased;
            () = &mut shutdown => break Ok(()),
            accept = listener.accept() => {
                let (stream, _) = match accept {
                    Ok(accepted) => accepted,
                    Err(err) => break Err(AppError::Platform(err.to_string())),
                };
                // Race permit acquisition against shutdown. Without this
                // arm, a saturated handler pool (32 in flight) would pin
                // the loop on `acquire_owned().await` and we would not
                // observe `shutdown` again until one of the in-flight
                // handlers freed a permit — which, in a degenerate case
                // where every handler is itself stuck on a slow DB write,
                // means the listener is not dropped until `drain_grace`
                // aborts those handlers. Selecting on shutdown here keeps
                // shutdown observation latency independent of handler
                // progress.
                let permit = tokio::select! {
                    biased;
                    () = &mut shutdown => {
                        // Refuse the just-accepted connection by dropping
                        // its stream; the client sees EOF and we proceed
                        // to drain stage on the next iteration.
                        drop(stream);
                        break Ok(());
                    }
                    permit = semaphore.clone().acquire_owned() => match permit {
                        Ok(permit) => permit,
                        Err(err) => break Err(AppError::Platform(format!(
                            "failed to acquire IPC connection permit: {err}"
                        ))),
                    },
                };
                let handler = handler.clone();
                let token = token.clone();
                tasks.spawn(handle_connection(stream, permit, handler, token));
            }
            // Reap completed handlers so the `JoinSet` doesn't grow without
            // bound for the lifetime of the daemon.
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
        }
    };

    // Stage 1: drop the listener so no further `accept()` succeeds even
    // for clients that beat the shutdown signal in.
    drop(listener);

    // Stage 2: wait up to `drain_grace` for in-flight handlers to commit.
    if !tasks.is_empty() {
        let drain = async { while tasks.join_next().await.is_some() {} };
        if timeout(drain_grace, drain).await.is_err() {
            // Stage 3: anything still running has had its grace period;
            // abort and reap so the JoinSet drops cleanly.
            warn!(
                grace_ms = u64::try_from(drain_grace.as_millis()).unwrap_or(u64::MAX),
                "ipc_drain_timeout_aborting_inflight",
            );
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        }
    }

    accept_result
}

/// Bounded-read + auth-check + write-back driver shared by every
/// transport. Generic over `AsyncRead + AsyncWrite` so the Unix-socket and
/// Windows named-pipe servers reuse the exact same envelope handling.
#[cfg(any(unix, windows))]
async fn handle_connection<S, F, Fut>(
    mut stream: S,
    permit: tokio::sync::OwnedSemaphorePermit,
    handler: Arc<F>,
    token: Arc<AuthToken>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    F: Fn(IpcRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = IpcResponse> + Send + 'static,
{
    let _permit = permit;
    // Bound the time we will hold a connection slot for a slow or stalled
    // client. Without this, an idle peer that never writes a newline would
    // pin one of the 32 semaphore permits forever.
    let line = match timeout(READ_TIMEOUT, read_bounded_line(&mut stream)).await {
        Ok(result) => result,
        Err(_) => Err("IPC request timed out".to_owned()),
    };
    let response = match line {
        Ok(line) => match serde_json::from_slice::<IpcEnvelope>(&line) {
            Ok(envelope) => {
                if token.verify(&envelope.token) {
                    handler(envelope.request).await
                } else {
                    IpcResponse::Error(crate::IpcError {
                        code: "unauthorized".to_owned(),
                        message: "invalid auth token".to_owned(),
                        recoverable: false,
                    })
                }
            }
            Err(err) => IpcResponse::Error(crate::IpcError {
                code: "invalid_request".to_owned(),
                message: err.to_string(),
                recoverable: true,
            }),
        },
        Err(err) => IpcResponse::Error(crate::IpcError {
            code: "invalid_request".to_owned(),
            message: err,
            recoverable: true,
        }),
    };
    let payload = match serde_json::to_vec(&response) {
        Ok(payload) if payload.len() < MAX_IPC_RESPONSE_BYTES => Some(payload),
        Ok(payload) => {
            // The daemon already paid the allocation by the time we get
            // here, so this branch protects the *wire* and the client's
            // bounded reader — not daemon RSS. Replace with a small error
            // envelope so the caller sees a structured rejection it can
            // act on (retry with a tighter limit) instead of timing out
            // on a truncated half-JSON.
            let oversized = IpcResponse::Error(crate::IpcError {
                code: "response_too_large".to_owned(),
                message: format!(
                    "response would be {} bytes, exceeds limit {}",
                    payload.len(),
                    MAX_IPC_RESPONSE_BYTES
                ),
                recoverable: false,
            });
            serde_json::to_vec(&oversized).ok()
        }
        Err(_) => None,
    };
    if let Some(payload) = payload {
        let _ = stream.write_all(&payload).await;
        let _ = stream.write_all(b"\n").await;
        // Best-effort flush so the client receives the response promptly
        // even on transports (named pipes) that buffer until shutdown.
        let _ = stream.flush().await;
    }
}

#[cfg(unix)]
pub async fn serve_unix<F, Fut>(path: impl AsRef<Path>, token: AuthToken, handler: F) -> Result<()>
where
    F: Fn(IpcRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = IpcResponse> + Send + 'static,
{
    let listener = bind_unix(path).await?;
    accept_loop(listener, token, handler).await
}

#[cfg(any(unix, windows))]
async fn read_bounded_line<R>(stream: &mut R) -> std::result::Result<Vec<u8>, String>
where
    R: AsyncRead + Unpin,
{
    let mut line = Vec::new();
    let mut chunk = [0_u8; 4096];
    let mut first_read = true;
    loop {
        // The first read gets a tight budget so a connecting peer that
        // never writes anything (slow-loris) cannot hold a permit for
        // the full `READ_TIMEOUT`; subsequent reads inherit the
        // surrounding `READ_TIMEOUT` set in `handle_connection`.
        let read = if first_read {
            match timeout(FIRST_READ_TIMEOUT, stream.read(&mut chunk)).await {
                Ok(result) => result.map_err(|err| err.to_string())?,
                Err(_) => return Err("IPC peer sent no data".to_owned()),
            }
        } else {
            stream
                .read(&mut chunk)
                .await
                .map_err(|err| err.to_string())?
        };
        first_read = false;
        if read == 0 {
            break;
        }
        if let Some(newline) = chunk[..read].iter().position(|byte| *byte == b'\n') {
            if line.len() + newline > MAX_IPC_REQUEST_BYTES {
                return Err("IPC request is too large".to_owned());
            }
            line.extend_from_slice(&chunk[..newline]);
            break;
        }
        if line.len() + read > MAX_IPC_REQUEST_BYTES {
            return Err("IPC request is too large".to_owned());
        }
        line.extend_from_slice(&chunk[..read]);
    }
    Ok(line)
}

// ---------------------------------------------------------------------------
// Windows named-pipe transport.
// ---------------------------------------------------------------------------

/// Default named-pipe name used by the Windows daemon.
///
/// Auth is enforced via the sibling token file rather than a custom DACL: the
/// pipe is created with the default named-pipe security descriptor inherited
/// from the daemon process, so any local caller who can also read the
/// `%LOCALAPPDATA%\nagori\nagori.token` file (written by the daemon under a
/// per-user roaming-equivalent directory) can authenticate. A future
/// hardening pass can attach an explicit `SECURITY_ATTRIBUTES` to restrict
/// the pipe to the current SID.
#[cfg(windows)]
pub const DEFAULT_PIPE_NAME: &str = r"\\.\pipe\nagori";

/// Build the `ServerOptions` baseline used for every `NamedPipeServer`
/// instance the daemon creates — first or chained. Centralised so the
/// remote-client rejection (the only piece of `DoS` mitigation that lives
/// in `ServerOptions` itself) can't be accidentally dropped on the
/// chained-instance path. Slow-loris pressure from *local* peers is
/// bounded by `FIRST_READ_TIMEOUT` / `READ_TIMEOUT` in
/// `handle_connection`, not by anything here.
#[cfg(windows)]
fn pipe_server_options() -> tokio::net::windows::named_pipe::ServerOptions {
    let mut opts = tokio::net::windows::named_pipe::ServerOptions::new();
    // `reject_remote_clients(true)` closes the UNC-path surface: without
    // it, a domain-joined peer could open `\\<host>\pipe\nagori` over
    // SMB and park a connection slot until the timeout elapses. Local
    // callers (which the default pipe DACL still admits) are bounded by
    // the read timeouts above instead.
    opts.reject_remote_clients(true);
    opts
}

/// Create the first instance of `pipe_name` synchronously.
///
/// Separated from `accept_loop_pipe_with_shutdown` so the daemon can fail
/// startup (rather than logging a warning from inside a spawned task) when
/// another process already publishes the same pipe name. The first instance
/// must carry `first_pipe_instance(true)` so the create errors out instead
/// of silently chaining onto somebody else's pipe.
#[cfg(windows)]
pub fn bind_pipe(pipe_name: &str) -> Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    pipe_server_options()
        .first_pipe_instance(true)
        .create(pipe_name)
        .map_err(|err| AppError::Platform(err.to_string()))
}

/// Three-stage graceful shutdown variant of the named-pipe accept loop,
/// modelled after [`accept_loop_with_shutdown`].
///
/// Named pipes do not have a separate `listen` / `accept` split: each
/// `NamedPipeServer` instance accepts at most one connection. Callers
/// pass in the already-bound first instance (see [`bind_pipe`]); the loop
/// allocates each subsequent instance after a successful connect so the
/// series stays continuous.
#[cfg(windows)]
pub async fn accept_loop_pipe_with_shutdown<F, Fut, S>(
    pipe_name: &str,
    first_instance: tokio::net::windows::named_pipe::NamedPipeServer,
    expected_token: AuthToken,
    handler: F,
    shutdown: S,
    drain_grace: Duration,
) -> Result<()>
where
    F: Fn(IpcRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = IpcResponse> + Send + 'static,
    S: Future<Output = ()> + Send,
{
    let handler = Arc::new(handler);
    let token = Arc::new(expected_token);
    let semaphore = Arc::new(Semaphore::new(32));
    let mut tasks: JoinSet<()> = JoinSet::new();

    // We hold the in-flight "next" instance behind `Option` so the
    // borrow checker accepts the move-then-replace pattern inside the
    // loop: on a successful connect we `take()` the connected handle
    // and immediately install the next one; on shutdown the remaining
    // `Some` is dropped to refuse further clients.
    let mut server = Some(first_instance);

    tokio::pin!(shutdown);
    let accept_result = loop {
        let current = server
            .as_mut()
            .expect("server slot is repopulated after every accept");
        tokio::select! {
            biased;
            () = &mut shutdown => break Ok(()),
            result = current.connect() => {
                if let Err(err) = result {
                    break Err(AppError::Platform(err.to_string()));
                }
                // Move the now-connected handle out and immediately
                // create the next listener so we keep accepting while
                // the worker runs.
                let connected = server.take().expect("connect resolved on an owned instance");
                // Every chained instance reuses the same baseline so the
                // remote-rejection bit can't drift between instances.
                server = match pipe_server_options().create(pipe_name) {
                    Ok(next) => Some(next),
                    Err(err) => break Err(AppError::Platform(err.to_string())),
                };
                let permit = tokio::select! {
                    biased;
                    () = &mut shutdown => {
                        drop(connected);
                        break Ok(());
                    }
                    permit = semaphore.clone().acquire_owned() => match permit {
                        Ok(permit) => permit,
                        Err(err) => break Err(AppError::Platform(format!(
                            "failed to acquire IPC connection permit: {err}"
                        ))),
                    },
                };
                let handler = handler.clone();
                let token = token.clone();
                tasks.spawn(handle_connection(connected, permit, handler, token));
            }
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
        }
    };

    // Drop the unconnected server (if any is still pending) so no
    // further clients can attach to this name.
    drop(server);

    if !tasks.is_empty() {
        let drain = async { while tasks.join_next().await.is_some() {} };
        if timeout(drain_grace, drain).await.is_err() {
            warn!(
                grace_ms = u64::try_from(drain_grace.as_millis()).unwrap_or(u64::MAX),
                "ipc_drain_timeout_aborting_inflight",
            );
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        }
    }
    accept_result
}

#[cfg(windows)]
pub async fn serve_pipe<F, Fut>(pipe_name: &str, token: AuthToken, handler: F) -> Result<()>
where
    F: Fn(IpcRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = IpcResponse> + Send + 'static,
{
    let first = bind_pipe(pipe_name)?;
    accept_loop_pipe_with_shutdown(
        pipe_name,
        first,
        token,
        handler,
        std::future::pending::<()>(),
        Duration::from_secs(0),
    )
    .await
}

#[cfg(all(test, unix))]
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
        let listener = bind_unix(&path).await.expect("bind");
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
        let listener = bind_unix(&path).await.expect("bind");

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
}

#[cfg(not(any(unix, windows)))]
pub async fn serve_unix<F, Fut>(
    _path: impl AsRef<Path>,
    _token: crate::AuthToken,
    _handler: F,
) -> Result<()>
where
    F: Fn(IpcRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = IpcResponse> + Send + 'static,
{
    Err(AppError::Unsupported(
        "IPC server is not available on this platform".to_owned(),
    ))
}

#[cfg(all(windows, not(unix)))]
pub async fn serve_unix<F, Fut>(
    _path: impl AsRef<Path>,
    _token: crate::AuthToken,
    _handler: F,
) -> Result<()>
where
    F: Fn(IpcRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = IpcResponse> + Send + 'static,
{
    Err(AppError::Unsupported(
        "Unix socket IPC is not available on Windows; use serve_pipe".to_owned(),
    ))
}

#[cfg(all(test, windows))]
mod tests_windows {
    use std::time::Duration;

    use super::*;
    use crate::{HealthResponse, IpcClient};

    fn test_token() -> AuthToken {
        AuthToken::generate().expect("token should generate")
    }

    fn unique_pipe_name(suffix: &str) -> String {
        format!(r"\\.\pipe\nagori-test-{}-{suffix}", std::process::id())
    }

    #[tokio::test]
    async fn round_trip_health_over_named_pipe() {
        let pipe = unique_pipe_name("health");
        let token = test_token();
        let server_pipe = pipe.clone();
        let server_token = token.clone();
        let server = tokio::spawn(async move {
            let _ = serve_pipe(&server_pipe, server_token, |request| async move {
                assert!(matches!(request, IpcRequest::Health));
                IpcResponse::Health(HealthResponse {
                    ok: true,
                    version: "pipe-test".to_owned(),
                    maintenance: crate::MaintenanceHealthReport::default(),
                })
            })
            .await;
        });

        // Give the server a beat to create its first pipe instance.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let client = IpcClient::new(pipe, token);
        let response = client
            .send(IpcRequest::Health)
            .await
            .expect("health round-trip over pipe");
        let IpcResponse::Health(health) = response else {
            panic!("expected health response, got {response:?}");
        };
        assert!(health.ok);
        assert_eq!(health.version, "pipe-test");
        server.abort();
    }
}
