//! Windows named-pipe transport: pipe creation with a current-user-only
//! DACL, the accept loop, and the `serve_pipe` entry point. The
//! per-connection envelope handling is shared with the Unix path via
//! [`super::connection`].

use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use nagori_core::{AppError, Result};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::timeout;
use tracing::warn;

use super::connection::handle_connection;
use super::health::{IpcServerConfig, IpcServerHealth, observe_handler_outcome};
use crate::AuthToken;
use crate::{IpcRequest, IpcResponse};

/// Default named-pipe name used by the Windows daemon.
///
/// Authentication is enforced both by an explicit DACL on the pipe
/// (current-user SID only — see [`pipe_security_handle`]) and by the
/// sibling token file. The token file ACL similarly restricts read access
/// to the current user, BUILTIN\Administrators, and NT AUTHORITY\SYSTEM.
pub const DEFAULT_PIPE_NAME: &str = r"\\.\pipe\nagori";

/// Build the `ServerOptions` baseline used for every `NamedPipeServer`
/// instance the daemon creates — first or chained. Centralised so the
/// remote-client rejection (the only piece of `DoS` mitigation that lives
/// in `ServerOptions` itself) can't be accidentally dropped on the
/// chained-instance path. Slow-loris pressure from *local* peers is
/// bounded by `FIRST_READ_TIMEOUT` / `READ_TIMEOUT` in
/// `handle_connection`, not by anything here.
fn pipe_server_options() -> tokio::net::windows::named_pipe::ServerOptions {
    let mut opts = tokio::net::windows::named_pipe::ServerOptions::new();
    // `reject_remote_clients(true)` closes the UNC-path surface: without
    // it, a domain-joined peer could open `\\<host>\pipe\nagori` over
    // SMB and park a connection slot until the timeout elapses. Local
    // callers are additionally restricted by the explicit DACL applied
    // through `create_with_security_attributes_raw` below.
    opts.reject_remote_clients(true);
    opts
}

/// Build a [`SecurityHandle`] suitable for a Windows named-pipe server:
/// DACL with a single ACE that grants the current user `GENERIC_READ |
/// GENERIC_WRITE` (and nothing to anyone else, including other local users
/// on the same desktop session).
fn pipe_security_handle() -> Result<crate::windows_security::SecurityHandle> {
    crate::windows_security::SecurityHandle::current_user_only(
        crate::windows_security::GENERIC_READ | crate::windows_security::GENERIC_WRITE,
    )
    .map_err(|err| AppError::Platform(format!("pipe security descriptor: {err}")))
}

/// Create a pipe instance bound to `pipe_name` with the current-user-only
/// DACL applied. `first` selects whether `first_pipe_instance(true)` is set,
/// which the initial instance must use to fail closed if another process is
/// already publishing the name.
#[allow(unsafe_code)]
fn create_pipe_instance(
    pipe_name: &str,
    first: bool,
) -> Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    let mut opts = pipe_server_options();
    if first {
        opts.first_pipe_instance(true);
    }
    let mut security = pipe_security_handle()?;
    let attrs_ptr = security.as_mut_ptr().cast::<std::ffi::c_void>();
    // SAFETY: `attrs_ptr` points at a valid SECURITY_ATTRIBUTES owned by
    // `security` for the duration of this call. Windows captures a copy
    // of the descriptor during `CreateNamedPipeW`, so `security` is safe
    // to drop right after the call returns.
    let server = unsafe { opts.create_with_security_attributes_raw(pipe_name, attrs_ptr) }
        .map_err(|err| AppError::Platform(err.to_string()))?;
    drop(security);
    Ok(server)
}

/// Create the first instance of `pipe_name` synchronously.
///
/// Separated from `accept_loop_pipe_with_shutdown` so the daemon can fail
/// startup (rather than logging a warning from inside a spawned task) when
/// another process already publishes the same pipe name. The first instance
/// must carry `first_pipe_instance(true)` so the create errors out instead
/// of silently chaining onto somebody else's pipe.
pub fn bind_pipe(pipe_name: &str) -> Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    create_pipe_instance(pipe_name, true)
}

/// Three-stage graceful shutdown variant of the named-pipe accept loop,
/// modelled after `accept_loop_with_shutdown` (the Unix-socket path).
///
/// Named pipes do not have a separate `listen` / `accept` split: each
/// `NamedPipeServer` instance accepts at most one connection. Callers
/// pass in the already-bound first instance (see [`bind_pipe`]); the loop
/// allocates each subsequent instance after a successful connect so the
/// series stays continuous.
// One argument over the lint's threshold: `pipe_name` is the extra knob the
// named-pipe path needs (to re-bind each chained instance) on top of the
// seven the Unix `accept_loop_with_shutdown` already carries.
#[allow(clippy::too_many_arguments)]
pub async fn accept_loop_pipe_with_shutdown<F, Fut, S>(
    pipe_name: &str,
    first_instance: tokio::net::windows::named_pipe::NamedPipeServer,
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
    // Stamp the active tuning onto the health snapshot (see Unix path
    // for the same rationale): doctor / health consumers should see the
    // active connection ceiling without an extra IPC roundtrip.
    server_health.record_config(config);
    // See `accept_loop_with_shutdown` (Unix path) for the rationale —
    // seed the liveness clock so the supervisor's wedge probe doesn't
    // measure against the UNIX epoch on an idle daemon.
    server_health.record_accept();

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
                // Same liveness-bump rationale as the Unix path: record
                // before allocating the next instance / permit so the
                // supervisor's wedge probe sees fresh accepts even when
                // the handler pool is saturated.
                server_health.record_accept();
                // Move the now-connected handle out and immediately
                // create the next listener so we keep accepting while
                // the worker runs.
                let connected = server.take().expect("connect resolved on an owned instance");
                // Every chained instance reuses the same baseline + the
                // explicit DACL so neither the remote-rejection bit nor
                // the per-user access restriction can drift between
                // instances.
                server = match create_pipe_instance(pipe_name, false) {
                    Ok(next) => Some(next),
                    Err(err) => break Err(err),
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
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                observe_handler_outcome(&server_health, result);
            }
        }
    };

    // Drop the unconnected server (if any is still pending) so no
    // further clients can attach to this name.
    drop(server);

    if !tasks.is_empty() {
        let drain = async {
            while let Some(result) = tasks.join_next().await {
                observe_handler_outcome(&server_health, result);
            }
        };
        if timeout(drain_grace, drain).await.is_err() {
            warn!(
                grace_ms = u64::try_from(drain_grace.as_millis()).unwrap_or(u64::MAX),
                "ipc_drain_timeout_aborting_inflight",
            );
            tasks.abort_all();
            while let Some(result) = tasks.join_next().await {
                observe_handler_outcome(&server_health, result);
            }
        }
    }
    accept_result
}

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
        IpcServerHealth::default(),
        IpcServerConfig::default(),
    )
    .await
}

/// Windows fallback for the cross-platform `serve_unix` entry point: there
/// is no Unix-domain socket on Windows, so callers must use [`serve_pipe`].
#[cfg(all(windows, not(unix)))]
pub async fn serve_unix<F, Fut>(
    _path: impl AsRef<Path>,
    _token: AuthToken,
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

#[cfg(test)]
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
                    capture: crate::CaptureHealthReport::default(),
                    ipc: crate::IpcHealthReport::default(),
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
