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
//!
//! **Leaked reads are single-flight.** A timed-out read closure keeps its
//! blocking thread (and the adapter's clipboard mutex) until the OS call
//! returns. The capture loop retries the same clipboard sequence on its next
//! tick, so a *permanently* wedged OS call would otherwise spawn one more
//! leaked thread per tick and eventually exhaust the tokio blocking pool —
//! stalling every later clipboard write, paste, DB job and the shutdown path
//! behind it. [`ClipboardReadGate`] caps that at one: while a previous read
//! closure is still running, a new read is refused with
//! [`BlockingError::Busy`] instead of being spawned, and the caller degrades
//! (the capture loop counts it as a failed tick and backs off).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// Single-flight admission for one adapter's blocking clipboard *reads*.
///
/// One gate per clipboard adapter, shared by every snapshot read that takes
/// the adapter's mutex. [`Self::run`] behaves like [`clipboard_blocking`] while
/// the gate is idle; if an earlier read closure is still on the blocking pool
/// — it timed out and its OS call has not returned — the new read is refused
/// with [`BlockingError::Busy`] *before* anything is spawned. That bounds the
/// leaked-thread accumulation described in the module docs at one per
/// adapter: a wedged host costs one blocking worker, not one per capture
/// tick.
///
/// The in-flight flag is released by a drop guard inside the closure, so it
/// clears whether the closure returns normally, late, or by panicking. The
/// cheap sequence-only poll (`current_sequence`) is deliberately *not* routed
/// through the gate: it does not take the mutex, and keeping it flowing is
/// what lets steady-state change detection continue through a hung body read.
#[derive(Debug, Clone, Default)]
pub struct ClipboardReadGate {
    in_flight: Arc<AtomicBool>,
}

/// Clears the gate when the closure finishes — by any path.
struct InFlightGuard(Arc<AtomicBool>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl ClipboardReadGate {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a previous read closure is still running on the blocking pool.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.in_flight.load(Ordering::Acquire)
    }

    /// Run a blocking clipboard read bounded by [`CLIPBOARD_OP_TIMEOUT`],
    /// refusing with [`BlockingError::Busy`] while an earlier read is still in
    /// flight.
    pub async fn run<F, T>(&self, op: &'static str, f: F) -> Result<T, BlockingError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        self.run_with_limit(op, CLIPBOARD_OP_TIMEOUT, f).await
    }

    /// [`Self::run`] with an injectable deadline so tests do not have to sit
    /// out the production window.
    async fn run_with_limit<F, T>(
        &self,
        op: &'static str,
        limit: Duration,
        f: F,
    ) -> Result<T, BlockingError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        if self
            .in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            tracing::warn!(op, "clipboard_read_refused_previous_read_still_in_flight");
            return Err(BlockingError::Busy { op });
        }
        let guard = InFlightGuard(Arc::clone(&self.in_flight));
        run_blocking_with_timeout(op, limit, move || {
            let _guard = guard;
            f()
        })
        .await
    }
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

/// Lock a clipboard adapter's mutex for a *read*, recovering from a poisoned
/// guard instead of erroring.
///
/// The mutex guards an `arboard::Clipboard` (or the in-memory fallback
/// buffer), neither of which carries a Rust-side invariant that a panic
/// mid-operation could leave half-updated: the next call starts a fresh
/// `OpenClipboard` / pasteboard read. So a poison flag here only means "some
/// earlier closure panicked while holding the lock", not "the guarded data is
/// now unsafe to touch". Propagating it as an error instead — the old
/// `lock_err` mapping — wedged *every* later capture / copy / paste behind one
/// historical panic until the process restarted. We recover the guard and
/// clear the flag so the adapter keeps working; the panic is still surfaced by
/// whatever unwound originally.
///
/// Reads are unbounded (no [`CLIPBOARD_OP_TIMEOUT`]) because a late read result
/// is simply discarded — see the module docs. Writes use
/// [`lock_clipboard_for_write`], which adds the lock-acquisition timeout.
pub fn lock_clipboard_recovering<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        mutex.clear_poison();
        tracing::warn!("clipboard_mutex_poison_recovered");
        poisoned.into_inner()
    })
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
/// A poisoned guard is recovered rather than reported as an error (see
/// [`lock_clipboard_recovering`] for why that is safe here), so a single
/// historical panic does not wedge every later copy-back behind it.
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
                // Recover rather than wedge every later copy-back behind one
                // historical panic — the guarded clipboard has no Rust
                // invariant a poison could mean is broken. Mirrors
                // `lock_clipboard_recovering`.
                mutex.clear_poison();
                tracing::warn!(op, "clipboard_mutex_poison_recovered");
                return Ok(err.into_inner());
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
    /// Refused before spawning: an earlier read on the same
    /// [`ClipboardReadGate`] timed out and its OS call has still not returned.
    /// The caller treats this like a failed tick; the read is retried once the
    /// wedged call unwinds and the gate clears.
    Busy {
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
            Self::Busy { op } => {
                format!("{op} refused: a previous clipboard read is still in flight")
            }
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
    async fn gate_refuses_a_second_read_while_the_first_is_wedged() {
        // Model a permanently wedged OS call: the first closure blocks on a
        // channel past its deadline and keeps its blocking thread. Every read
        // admitted while it is still running must be refused *without*
        // spawning — the whole point is that the pool does not accumulate one
        // leaked thread per capture tick.
        let gate = ClipboardReadGate::new();
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let started_first = Arc::clone(&started);
        let err = gate
            .run_with_limit("wedged", Duration::from_millis(50), move || {
                started_first.fetch_add(1, Ordering::SeqCst);
                let _ = rx.recv();
            })
            .await
            .expect_err("the wedged read must time out");
        assert!(matches!(err, BlockingError::Timeout { op: "wedged", .. }));
        assert!(gate.is_busy(), "the leaked closure still owns the gate");

        for _ in 0..3 {
            let started_next = Arc::clone(&started);
            let err = gate
                .run_with_limit("retry", Duration::from_secs(5), move || {
                    started_next.fetch_add(1, Ordering::SeqCst);
                })
                .await
                .expect_err("reads must be refused while the first is in flight");
            assert!(matches!(err, BlockingError::Busy { op: "retry" }));
        }
        assert_eq!(
            started.load(Ordering::SeqCst),
            1,
            "refused reads must never reach the blocking pool"
        );

        // Unrelated blocking work is unaffected by the wedged read.
        let unrelated = run_blocking_with_timeout("unrelated", Duration::from_secs(5), || 3_u8)
            .await
            .expect("an unrelated blocking job completes while a read is wedged");
        assert_eq!(unrelated, 3);

        // Release the wedged OS call; the gate clears and the next read runs.
        drop(tx);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while gate.is_busy() {
            assert!(
                std::time::Instant::now() < deadline,
                "the gate must clear once the wedged closure returns"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let value = gate
            .run_with_limit("after", Duration::from_secs(5), || 9_u8)
            .await
            .expect("a read after the gate cleared must run");
        assert_eq!(value, 9);
        assert_eq!(started.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn gate_clears_after_a_panicking_read() {
        // The drop guard must release the gate even when the closure unwinds;
        // otherwise one panic would refuse every later capture until restart.
        let gate = ClipboardReadGate::new();
        let err = gate
            .run("boom", || -> u8 { panic!("read closure blew up") })
            .await
            .expect_err("a panicking read surfaces as Panicked");
        assert!(matches!(err, BlockingError::Panicked { op: "boom" }));
        assert!(!gate.is_busy(), "a panic must not leave the gate held");
        let value = gate.run("next", || 4_u8).await.expect("next read runs");
        assert_eq!(value, 4);
    }

    #[tokio::test]
    async fn gate_admits_sequential_reads() {
        let gate = ClipboardReadGate::new();
        for expected in 0..3_u8 {
            let value = gate
                .run("sequential", move || expected)
                .await
                .expect("idle gate admits every read");
            assert_eq!(value, expected);
            assert!(!gate.is_busy());
        }
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

    #[test]
    fn recovering_lock_returns_the_guard_after_a_poisoning_panic() {
        // A panic while holding the guard poisons the mutex. The recovering
        // read lock must hand the guard back rather than wedging every later
        // capture behind the historical panic.
        use std::sync::{Arc, Mutex};

        let mutex = Arc::new(Mutex::new(7_u32));
        let poison = mutex.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poison.lock().expect("holder lock");
            panic!("poison the mutex");
        })
        .join();
        assert!(
            mutex.is_poisoned(),
            "the panic must have poisoned the mutex"
        );

        let guard = lock_clipboard_recovering(&mutex);
        assert_eq!(*guard, 7, "the recovered guard still sees the value");
        drop(guard);
        assert!(
            !mutex.is_poisoned(),
            "recovery clears the poison so later locks succeed cleanly"
        );
    }

    #[test]
    fn write_lock_recovers_a_poisoned_guard() {
        // The write-side lock acquisition must also recover from poison —
        // otherwise a single panic wedges every copy-back until restart.
        use std::sync::{Arc, Mutex};

        let mutex = Arc::new(Mutex::new(()));
        let poison = mutex.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poison.lock().expect("holder lock");
            panic!("poison the mutex");
        })
        .join();
        assert!(
            mutex.is_poisoned(),
            "the panic must have poisoned the mutex"
        );

        let guard = lock_for_write_with_limit(&mutex, "test_write", Duration::from_millis(50))
            .expect("a poisoned guard must be recovered, not errored");
        drop(guard);
        assert!(!mutex.is_poisoned(), "recovery clears the poison");
    }
}
