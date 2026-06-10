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
