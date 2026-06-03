//! Timeout-bounded blocking-pool execution for platform adapters.
//!
//! Focus restore, frontmost-app probing, and clipboard copy-back all funnel
//! synchronous OS calls (`activateWithOptions` / `SetForegroundWindow`,
//! `NSWorkspace` reads, `wl-clipboard` offers) onto the tokio blocking pool. A
//! healthy call answers in milliseconds, but a wedged `AppKit` / `USER32` lock
//! or a frozen Wayland compositor would otherwise leave the bare
//! `spawn_blocking().await` pending forever — freezing the paste serialisation
//! and the UI toast that surfaces the result.
//!
//! [`run_blocking_with_timeout`] bounds each call so the async caller always
//! gets a result back within the window. It mirrors the macOS clipboard
//! adapter's `clipboard_blocking` and the Linux paste adapter's
//! `WTYPE_TIMEOUT`, generalised so the window / clipboard adapters on all
//! three hosts share one implementation.
//!
//! **Caveat — the timeout does not cancel the closure.** `spawn_blocking`
//! tasks cannot be aborted, so on timeout the detached thread keeps running
//! and the OS call still completes once it un-wedges. That is fine when a late
//! completion is harmless: a clipboard write lands the *intended* content, a
//! focus restore re-foregrounds the source app the user came from, and a
//! frontmost-app probe's result is simply discarded. It is **not** safe for an
//! op whose late side effect would be harmful — most notably synthetic paste,
//! where a stray `⌘V` / `Ctrl+V` after the user has moved on would inject
//! clipboard content into an unrelated window. Synthetic-input synthesis is
//! therefore awaited *without* a timeout in the paste adapters (the Linux path
//! is the exception: it shells out to `wtype` and kills the subprocess on
//! timeout, which is a real cancellation).

use std::time::Duration;

/// Why a blocking platform op did not return a value.
///
/// Both variants mean "the closure produced no result"; callers map them onto
/// their own domain error (`PasteFailureReason::Timeout`,
/// `AppError::Platform`, …) and fall back to manual paste / degraded health.
#[derive(Debug)]
pub enum BlockingError {
    /// The OS call did not return within the deadline. The detached blocking
    /// thread is *leaked* until the call finally unwedges — `spawn_blocking`
    /// tasks cannot be aborted — but that is acceptable for the realistic
    /// transient hang: the thread frees itself when the call returns.
    Timeout {
        /// Stable op label for logs / messages.
        op: &'static str,
        /// The deadline that elapsed.
        limit: Duration,
    },
    /// The blocking closure panicked on the pool. Surfaced rather than
    /// re-panicked so a single bad call does not take down a worker.
    Panicked {
        /// Stable op label for logs / messages.
        op: &'static str,
    },
}

impl BlockingError {
    /// Human-readable detail reused in the adapters' error messages.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Timeout { op, limit } => {
                format!("{op} did not return within {}s", limit.as_secs_f32())
            }
            Self::Panicked { op } => format!("{op} task panicked"),
        }
    }
}

impl std::fmt::Display for BlockingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe())
    }
}

impl std::error::Error for BlockingError {}

/// Run `f` on the blocking pool, bounded by `limit`.
///
/// Returns the closure's value on success, or a [`BlockingError`] when the OS
/// call timed out or the closure panicked. A timeout is logged at `warn` and a
/// panic at `error`, keyed on `op`, so a wedged host call leaves a breadcrumb
/// without the caller having to log at every site.
pub async fn run_blocking_with_timeout<F, T>(
    op: &'static str,
    limit: Duration,
    f: F,
) -> Result<T, BlockingError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    match tokio::time::timeout(limit, tokio::task::spawn_blocking(f)).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(join_err)) => {
            tracing::error!(op, error = %join_err, "platform_blocking_op_panicked");
            Err(BlockingError::Panicked { op })
        }
        Err(_elapsed) => {
            tracing::warn!(
                op,
                timeout_ms = u64::try_from(limit.as_millis()).unwrap_or(u64::MAX),
                "platform_blocking_op_timed_out",
            );
            Err(BlockingError::Timeout { op, limit })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_the_closure_value_on_success() {
        let value = run_blocking_with_timeout("ok", Duration::from_secs(5), || 7_u32)
            .await
            .expect("fast closure must not time out");
        assert_eq!(value, 7);
    }

    #[tokio::test]
    async fn maps_a_wedged_op_to_timeout() {
        // Model a wedged OS call: the closure blocks on a channel until the
        // test releases it. A short *real* limit exercises the elapsed branch
        // without sleeping out a production window — paused time can't be used
        // here because tokio won't auto-advance the clock while a
        // `spawn_blocking` task is still pending on the pool.
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let limit = Duration::from_millis(50);
        let start = std::time::Instant::now();
        let err = run_blocking_with_timeout("wedged", limit, move || {
            let _ = rx.recv();
        })
        .await
        .expect_err("a closure that overruns the deadline must time out");
        assert!(matches!(err, BlockingError::Timeout { op: "wedged", .. }));
        assert!(
            start.elapsed() >= limit,
            "the timeout must elapse before giving up, not fail fast",
        );
        // Release the blocking worker so it returns instead of blocking on
        // `recv` until the test process exits.
        drop(tx);
    }

    #[tokio::test]
    async fn maps_a_panicking_closure_to_panicked() {
        let err = run_blocking_with_timeout("boom", Duration::from_secs(5), || {
            panic!("closure blew up");
        })
        .await
        .expect_err("a panicking closure must surface as Panicked");
        assert!(matches!(err, BlockingError::Panicked { op: "boom" }));
    }
}
