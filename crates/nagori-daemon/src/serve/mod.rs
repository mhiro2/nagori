//! Daemon serving entry points: the IPC supervisor and the daemon lifecycle.
//!
//! Split across [`ipc`] (bind / accept-loop supervision / runtime-file
//! cleanup) and [`lifecycle`] (`run_daemon`, signal handling, worker
//! supervision, drain). The [`drain_one`] task-drain primitive lives here
//! because both halves use it.

mod ipc;
mod lifecycle;

pub use ipc::{CliIpcConfig, default_socket_path, spawn_cli_ipc_supervisor};
pub use lifecycle::{
    DaemonConfig, WorkerRestart, acquire_data_dir_lock, run_daemon, supervise_worker,
};

use std::time::Duration;

use tracing::warn;

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

#[cfg(all(test, unix))]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn drain_one_aborts_a_worker_that_overruns_the_grace() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        // A worker that only "finishes" after an hour must be aborted once it
        // overruns the short drain grace, not awaited to completion — otherwise
        // shutdown would hang on a wedged worker. With paused time the runtime
        // advances to the nearest timer (the 50 ms grace) first, so the
        // timeout→abort path fires deterministically.
        let finished = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&finished);
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_hours(1)).await;
            flag.store(true, Ordering::SeqCst);
        });

        drain_one("test_worker", handle, Duration::from_millis(50)).await;

        assert!(
            !finished.load(Ordering::SeqCst),
            "a worker that overruns the grace must be aborted before it completes",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn drain_one_joins_a_worker_that_finishes_within_the_grace() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        // The happy path: a worker that completes inside the grace is joined
        // cleanly rather than aborted, so its final work (here, setting the
        // flag) is allowed to land.
        let finished = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&finished);
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            flag.store(true, Ordering::SeqCst);
        });

        drain_one("test_worker", handle, Duration::from_mins(1)).await;

        assert!(
            finished.load(Ordering::SeqCst),
            "a worker that finishes within the grace must run to completion",
        );
    }
}
