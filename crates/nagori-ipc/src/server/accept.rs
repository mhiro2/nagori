//! Accept-loop scaffolding shared by the Unix-socket and named-pipe
//! transports: the per-accept permit acquisition raced against shutdown,
//! and the post-loop two-stage handler drain.
//!
//! The accept mechanisms themselves stay platform-specific — a Unix
//! listener accepts many connections, while a named-pipe instance accepts
//! exactly one and is re-created per connect — so only the
//! transport-agnostic skeleton lives here.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use nagori_core::{AppError, Result};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use tokio::time::timeout;
use tracing::warn;

use super::health::{IpcServerHealth, observe_handler_outcome};

/// Backoff before the accept loop retries after a transient accept error.
///
/// Long enough that an fd-exhaustion storm cannot spin the loop hot, short
/// enough that a recovered listener resumes accepting promptly. The sleep is
/// raced against shutdown at the call sites so a draining daemon never waits
/// it out.
pub(super) const ACCEPT_RETRY_BACKOFF: Duration = Duration::from_millis(100);

/// Whether an accept-stage I/O error is transient — i.e. expected to resolve
/// on its own without rebinding the listener.
///
/// `accept(2)` routinely surfaces fd exhaustion (`EMFILE`/`ENFILE`),
/// connections aborted by the peer before we accepted them
/// (`ECONNABORTED`), interrupted syscalls, and kernel memory pressure.
/// None of these say anything about the listener itself; breaking the
/// accept loop on them would take the whole IPC surface down — and pay a
/// supervisor respawn plus re-bind — over a single hiccup. Errors that *do*
/// indicate a broken listener (`EBADF`, `EINVAL`, …) stay fatal so the
/// supervisor can rebuild it.
pub(super) fn is_transient_accept_error(err: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    if matches!(
        err.kind(),
        ErrorKind::ConnectionAborted
            | ErrorKind::ConnectionReset
            | ErrorKind::Interrupted
            | ErrorKind::WouldBlock
            | ErrorKind::OutOfMemory
    ) {
        return true;
    }
    // Resource-exhaustion codes have no stable `ErrorKind` mapping, so
    // match the raw OS codes per platform.
    #[cfg(unix)]
    if let Some(code) = err.raw_os_error() {
        return matches!(
            code,
            libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM | libc::EPROTO
        );
    }
    #[cfg(windows)]
    if let Some(code) = err.raw_os_error() {
        // ERROR_TOO_MANY_OPEN_FILES (4), ERROR_NOT_ENOUGH_MEMORY (8),
        // ERROR_OUTOFMEMORY (14), ERROR_NO_DATA (232: the client connected
        // and disconnected before the server-side connect observed it),
        // ERROR_NO_SYSTEM_RESOURCES (1450).
        return matches!(code, 4 | 8 | 14 | 232 | 1450);
    }
    false
}

/// Race handler-permit acquisition against shutdown.
///
/// Without this race, a saturated handler pool would pin the accept loop on
/// `acquire_owned().await` and shutdown would not be observed again until
/// one of the in-flight handlers freed a permit — which, in a degenerate
/// case where every handler is itself stuck on a slow DB write, means the
/// listener is not dropped until `drain_grace` aborts those handlers.
/// Selecting on shutdown here keeps shutdown observation latency
/// independent of handler progress.
///
/// Returns `Ok(None)` when shutdown fired first — the caller refuses the
/// just-accepted connection (dropping its stream so the client sees EOF)
/// and proceeds to the drain stage.
pub(super) async fn acquire_permit_or_shutdown<S>(
    shutdown: Pin<&mut S>,
    semaphore: Arc<Semaphore>,
) -> Result<Option<OwnedSemaphorePermit>>
where
    S: Future<Output = ()> + Send,
{
    tokio::select! {
        biased;
        () = shutdown => Ok(None),
        permit = semaphore.acquire_owned() => match permit {
            Ok(permit) => Ok(Some(permit)),
            Err(err) => Err(AppError::Platform(format!(
                "failed to acquire IPC connection permit: {err}"
            ))),
        },
    }
}

/// Stages 2 and 3 of the accept loops' graceful shutdown.
///
/// Stage 2: wait up to `drain_grace` for the spawned handlers to finish so
/// in-flight transactions get a chance to commit instead of being
/// half-applied. Stage 3: anything still running after the grace is
/// aborted, and the abort results are reaped through
/// [`observe_handler_outcome`] so a panicking handler is logged and counted
/// in [`IpcServerHealth`] instead of being silently dropped.
pub(super) async fn drain_handlers(
    mut tasks: JoinSet<()>,
    drain_grace: Duration,
    server_health: &IpcServerHealth,
) {
    if tasks.is_empty() {
        return;
    }
    let drain = async {
        while let Some(result) = tasks.join_next().await {
            observe_handler_outcome(server_health, result);
        }
    };
    if timeout(drain_grace, drain).await.is_err() {
        warn!(
            grace_ms = u64::try_from(drain_grace.as_millis()).unwrap_or(u64::MAX),
            "ipc_drain_timeout_aborting_inflight",
        );
        tasks.abort_all();
        while let Some(result) = tasks.join_next().await {
            observe_handler_outcome(server_health, result);
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::*;

    #[cfg(unix)]
    #[test]
    fn resource_exhaustion_and_aborted_connections_are_transient() {
        for code in [
            libc::EMFILE,
            libc::ENFILE,
            libc::ECONNABORTED,
            libc::EINTR,
            libc::ENOBUFS,
            libc::ENOMEM,
        ] {
            let err = std::io::Error::from_raw_os_error(code);
            assert!(
                is_transient_accept_error(&err),
                "code {code} ({err}) must be retried, not kill the accept loop",
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn broken_listener_errors_stay_fatal() {
        // A closed/invalid listener fd means retrying can never succeed —
        // the loop must exit so the supervisor rebinds.
        for code in [libc::EBADF, libc::EINVAL, libc::ENOTSOCK] {
            let err = std::io::Error::from_raw_os_error(code);
            assert!(
                !is_transient_accept_error(&err),
                "code {code} ({err}) must break the accept loop",
            );
        }
    }
}
