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
//! completion is harmless: a focus restore re-foregrounds the source app the
//! user came from, and a frontmost-app probe's result is simply discarded. It
//! is **not** safe for an op whose late side effect would be harmful:
//!
//! - **Synthetic paste** — a stray `⌘V` / `Ctrl+V` after the user has moved
//!   on would inject clipboard content into an unrelated window. Synthetic-
//!   input synthesis is therefore awaited *without* a timeout in the paste
//!   adapters (the Linux path is the exception: it shells out to `wtype` and
//!   kills the subprocess on timeout, which is a real cancellation).
//! - **Clipboard write (copy-back)** — a timed-out write would still land on
//!   the OS clipboard once it un-wedges, overwriting whatever the user copied
//!   in the meantime and clobbering newer (possibly sensitive) content. The
//!   platform clipboard adapters therefore await their *write* paths to
//!   completion without this timeout (`clipboard_write_blocking` on macOS /
//!   Windows, the timeout-free `run_clipboard_write` on Linux), and reserve
//!   the timeout for *reads*, whose late result is simply discarded.

use std::time::Duration;

/// Upper bound on a single blocking clipboard *read* operation.
///
/// arboard + OS clipboard calls run on the blocking pool via
/// `spawn_blocking`. A healthy clipboard answers in milliseconds, but a
/// wedged host clipboard (a frozen source app mid-publish on macOS, a
/// foreground app that never calls `CloseClipboard` on Windows) would
/// otherwise pin the blocking worker — and any clipboard mutex guard it
/// holds — indefinitely, cascading into every later capture / copy / paste
/// that needs the same lock. Capping each read keeps the daemon's async flow
/// responsive: it always gets a degraded result back within the window. This
/// mirrors the Linux adapter's internal `PIPE_READ_TIMEOUT`.
///
/// On timeout the detached blocking thread (and any mutex guard it holds) is
/// leaked until the OS call finally unwedges — `spawn_blocking` tasks cannot
/// be aborted. That is acceptable for the realistic *transient* hang: the
/// thread frees itself when the call returns, and the sequence-only poll
/// path does not take the mutex, so steady-state change detection keeps
/// working through a hung body read.
pub const CLIPBOARD_OP_TIMEOUT: Duration = Duration::from_secs(3);

/// Run a blocking clipboard *read* on the blocking pool, bounded by
/// [`CLIPBOARD_OP_TIMEOUT`].
///
/// Drop-in replacement for `tokio::task::spawn_blocking` at the adapters'
/// call sites: the returned future still resolves to `Result<T, _>` so the
/// existing `.await.map_err(..)` tail is unchanged, but a wedged OS call now
/// resolves to [`BlockingError::Timeout`] instead of hanging forever. A late
/// read result is simply discarded, so the leaked-thread caveat above
/// applies harmlessly here.
pub async fn clipboard_blocking<F, T>(op: &'static str, f: F) -> Result<T, BlockingError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    run_blocking_with_timeout(op, CLIPBOARD_OP_TIMEOUT, f).await
}

/// Run a *side-effecting* clipboard write on the blocking pool, awaited to
/// completion — deliberately **without** [`CLIPBOARD_OP_TIMEOUT`].
///
/// A timeout would be unsafe here. `spawn_blocking` tasks cannot be aborted,
/// so a timed-out write would not stop: the detached thread keeps running and
/// still lands on the OS clipboard once the call unwedges, overwriting
/// whatever the user copied in the meantime — silently clobbering newer (and
/// possibly sensitive) clipboard content. We therefore await the write to
/// completion, so the caller either learns the clipboard truly holds the
/// intended content or blocks until a wedged clipboard recovers. This mirrors
/// the synthetic-paste contract of [`run_blocking_with_timeout`]'s module
/// docs. Reads keep [`clipboard_blocking`] because a late read result is
/// simply discarded.
pub async fn clipboard_write_blocking<F, T>(op: &'static str, f: F) -> Result<T, BlockingError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(value) => Ok(value),
        Err(join_err) => {
            tracing::error!(op, error = %join_err, "platform_blocking_op_panicked");
            Err(BlockingError::Panicked { op })
        }
    }
}

/// Acquire a clipboard adapter's mutex for a *write*, bounded by
/// [`CLIPBOARD_OP_TIMEOUT`].
///
/// Write closures deliberately run without the operation timeout (see
/// [`clipboard_write_blocking`]): once the OS side effect starts it must run
/// to completion. But the *lock acquisition* in front of it has no side
/// effect — and a guard leaked by a timed-out read (the detached blocking
/// thread keeps holding the `Mutex` until the wedged OS call returns) would
/// otherwise park a plain `lock()` here indefinitely, freezing every later
/// copy-back / paste behind a single wedged read. Bounding only the lock
/// stage preserves the no-timeout write contract: failing here touches
/// nothing, and once the guard is held the OS write still runs unbounded.
///
/// Poisoning is reported as an error exactly like the `lock_err` mapping the
/// adapters used before.
pub fn lock_clipboard_for_write<'a, T>(
    mutex: &'a std::sync::Mutex<T>,
    op: &'static str,
) -> nagori_core::Result<std::sync::MutexGuard<'a, T>> {
    lock_for_write_with_limit(mutex, op, CLIPBOARD_OP_TIMEOUT)
}

/// [`lock_clipboard_for_write`] with an injectable deadline so tests do not
/// have to sit out the production window.
fn lock_for_write_with_limit<'a, T>(
    mutex: &'a std::sync::Mutex<T>,
    op: &'static str,
    limit: Duration,
) -> nagori_core::Result<std::sync::MutexGuard<'a, T>> {
    /// Poll interval between `try_lock` attempts. Coarse enough to stay
    /// invisible next to OS clipboard latency, fine enough that a freed
    /// guard is picked up promptly.
    const LOCK_RETRY: Duration = Duration::from_millis(10);

    let deadline = std::time::Instant::now() + limit;
    loop {
        match mutex.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::Poisoned(err)) => {
                return Err(nagori_core::AppError::Platform(err.to_string()));
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                if std::time::Instant::now() >= deadline {
                    tracing::warn!(
                        op,
                        timeout_ms = u64::try_from(limit.as_millis()).unwrap_or(u64::MAX),
                        "clipboard_write_lock_timed_out",
                    );
                    return Err(nagori_core::AppError::Platform(format!(
                        "{op}: clipboard lock not acquired within {}s — a previous \
                         clipboard operation is still holding it",
                        limit.as_secs_f32()
                    )));
                }
                std::thread::sleep(LOCK_RETRY);
            }
        }
    }
}

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
    async fn clipboard_write_blocking_returns_the_closure_value() {
        let value = clipboard_write_blocking("write_ok", || 11_u32)
            .await
            .expect("write closure must complete");
        assert_eq!(value, 11);
    }

    #[tokio::test]
    async fn clipboard_write_blocking_maps_a_panicking_closure_to_panicked() {
        let err = clipboard_write_blocking("write_boom", || -> u32 {
            panic!("write closure blew up");
        })
        .await
        .expect_err("a panicking write closure must surface as Panicked");
        assert!(matches!(err, BlockingError::Panicked { op: "write_boom" }));
    }

    #[test]
    fn write_lock_times_out_while_another_thread_holds_the_guard() {
        // Model the leaked-read-guard scenario: a detached thread holds the
        // clipboard mutex past the deadline. The write-side lock must give
        // up with an error instead of parking forever.
        use std::sync::{Arc, Mutex};

        let mutex = Arc::new(Mutex::new(()));
        let holder_mutex = mutex.clone();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let _guard = holder_mutex.lock().expect("holder lock");
            held_tx.send(()).expect("signal held");
            let _ = release_rx.recv();
        });
        held_rx
            .recv()
            .expect("guard must be held before the attempt");

        let err = lock_for_write_with_limit(&mutex, "test_write", Duration::from_millis(50))
            .expect_err("a held guard must time the write lock out");
        assert!(
            err.to_string().contains("not acquired"),
            "unexpected error: {err}"
        );

        // Release the holder; the next acquisition must succeed promptly.
        release_tx.send(()).expect("release holder");
        holder.join().expect("holder thread");
        let _guard = lock_for_write_with_limit(&mutex, "test_write", Duration::from_millis(50))
            .expect("freed guard must be acquirable");
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
