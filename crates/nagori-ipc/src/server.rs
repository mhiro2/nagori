use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::{future::Future, path::Path};

use nagori_core::{AppError, Result};
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
use crate::{IpcEnvelope, IpcHealthReport, IpcRequest, IpcResponse};

/// Tunables for the IPC server.
///
/// Carried separately from [`IpcServerHealth`] so a single struct can be
/// passed through startup paths (daemon `serve.rs`, integration tests,
/// doctor) without coupling tuning knobs to the observer counters.
#[derive(Debug, Clone, Copy)]
pub struct IpcServerConfig {
    /// Maximum number of per-connection handlers in flight at once.
    /// Backed by an in-process [`tokio::sync::Semaphore`]; the 33rd
    /// concurrent client waits on `acquire_owned` until a handler frees
    /// a permit. Sized as a `NonZeroUsize` so an accidental `0` (which
    /// would deadlock every connection) is rejected at construction.
    pub max_concurrent_connections: NonZeroUsize,
}

impl IpcServerConfig {
    /// Default ceiling for in-flight IPC handlers. Sized for the local
    /// CLI / desktop workload where a handful of concurrent connections
    /// is typical and a saturated pool is more likely a sign of a wedged
    /// handler than legitimate fan-out.
    pub const DEFAULT_MAX_CONCURRENT_CONNECTIONS: usize = 32;
}

impl Default for IpcServerConfig {
    fn default() -> Self {
        // `NonZeroUsize::new` is `Option`; the const guarantees the
        // unwrap is infallible.
        Self {
            max_concurrent_connections: NonZeroUsize::new(Self::DEFAULT_MAX_CONCURRENT_CONNECTIONS)
                .expect("DEFAULT_MAX_CONCURRENT_CONNECTIONS must be non-zero"),
        }
    }
}

/// Capacity of the message-history ring buffer carried by
/// [`IpcServerHealth`]. The latest 8 redacted panic messages are kept for
/// `nagori doctor` triage; the time-window count is tracked separately so
/// it doesn't saturate at this capacity.
const PANIC_RING_CAPACITY: usize = 8;

/// Window used by [`IpcServerHealth::panics_last_5m`]. Operators reading
/// `nagori health` care less about the per-process total (which a panic
/// loop saturates within seconds) and more about whether new panics are
/// still landing right now.
const PANICS_WINDOW: Duration = Duration::from_mins(5);

/// Upper bound on the panic-timestamp window deque. Sized to swallow ~13
/// panics/second over the full [`PANICS_WINDOW`] before the head starts
/// evicting; well beyond any realistic panic-loop rate (each panic carries
/// at least a connection setup + tokio task teardown), so the saturation
/// path is reserved for outright pathology. Without this cap, a runaway
/// panic loop would inflate the deque without bound between probes.
const PANIC_WINDOW_MAX: usize = 4096;

/// One entry in the recent-panics message ring. The string has already
/// been routed through [`redact_panic_message`] so token-like hex runs
/// never reach the wire. Timestamps for the 5-minute rate live in
/// `panic_window` rather than alongside the message — keeping them
/// separate lets the window grow past the message-ring's small capacity
/// without dragging every redacted string with it.
#[derive(Debug, Clone)]
struct PanicEntry {
    message: String,
}

/// Cloneable observer for IPC server-side handler outcomes.
///
/// Per-connection handlers run on a `JoinSet`. Without an explicit
/// observer the `join_next()` reap drops the `Result`, so a panicking
/// handler is invisible in logs and in `nagori doctor` / `nagori health`.
/// `IpcServerHealth` lets the daemon thread a shared counter through the
/// accept loops so panics are both warned and surfaced in the health
/// report.
#[derive(Debug, Clone)]
pub struct IpcServerHealth {
    inner: Arc<IpcServerHealthInner>,
}

impl Default for IpcServerHealth {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
struct IpcServerHealthInner {
    handler_panics: AtomicU64,
    /// Wall-clock millis-since-epoch of the most recent `listener.accept()`
    /// (Unix) / `NamedPipeServer::connect()` (Windows) completion. Zero
    /// when no accept has been observed yet. The daemon's supervisor uses
    /// this — combined with periodic self-probes — to detect an accept
    /// loop that wedged on the OS side (handler deadlock, kernel-level
    /// resource exhaustion) without exiting the spawned task. Daemon
    /// liveness alone would miss that class of failure because the
    /// supervisor only respawns on task exit, not on silent input
    /// starvation.
    last_accept_at_ms: AtomicU64,
    /// Snapshot of the active [`IpcServerConfig::max_concurrent_connections`].
    /// `0` until the accept loop initialises it, which lets readers
    /// (`nagori doctor`, `nagori health`) render "(unknown)" when the
    /// daemon has not yet finished startup. Atomic so the loop can
    /// stamp it before recording the first accept without taking a
    /// lock; the value is otherwise immutable for the loop's lifetime.
    max_concurrent_connections: AtomicUsize,
    /// Bounded ring of the most recent panic messages for `nagori doctor`
    /// triage. Each entry stores a redacted message (see
    /// [`redact_panic_message`]). Strictly the most recent
    /// [`PANIC_RING_CAPACITY`] panics — the 5-minute count lives in
    /// `panic_window` so it can exceed this ring's capacity.
    panic_ring: Mutex<VecDeque<PanicEntry>>,
    /// Timestamps (millis-since-epoch) of every panic observed in the
    /// last [`PANICS_WINDOW`], capped at [`PANIC_WINDOW_MAX`]. Pruned by
    /// timestamp on every push and every read so the deque size tracks
    /// the active panic rate. Separate from `panic_ring` so a tight
    /// panic loop with more than [`PANIC_RING_CAPACITY`] hits inside the
    /// window doesn't get under-reported by `panics_last_5m`.
    panic_window: Mutex<VecDeque<u64>>,
}

impl IpcServerHealth {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(IpcServerHealthInner::default()),
        }
    }

    /// Total number of IPC handler tasks observed to panic over this
    /// server's lifetime. Backed by a `fetch_add(Relaxed)` so the counter
    /// wraps at `u64::MAX`; in practice the daemon will exit long before
    /// that ever happens, so we accept the wrap instead of paying for a
    /// CAS loop on every panic record.
    #[must_use]
    pub fn handler_panic_count(&self) -> u64 {
        self.inner.handler_panics.load(Ordering::Relaxed)
    }

    /// Most recent panic message, if any. Reads the back of the ring
    /// buffer so the "latest" view and the recent-window count can never
    /// drift; cloned so the lock is held only long enough to copy out.
    #[must_use]
    pub fn last_panic_message(&self) -> Option<String> {
        let guard = match self.inner.panic_ring.lock() {
            Ok(guard) => guard,
            // A panic while holding the mutex must not silence the
            // health surface forever — recover the inner value and keep
            // serving snapshots, matching `MaintenanceHealth`.
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.back().map(|entry| entry.message.clone())
    }

    /// Count of panic events recorded within the last [`PANICS_WINDOW`].
    /// A non-zero value here is the actionable signal for operators:
    /// the cumulative `handler_panic_count` plateaus quickly under a
    /// panic loop, while this window slides so dashboards can show
    /// "still failing right now" vs. "one fluke an hour ago".
    #[must_use]
    pub fn panics_last_5m(&self) -> u32 {
        let cutoff_ms = window_cutoff_ms();
        let mut guard = match self.inner.panic_window.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        // Prune on read so readers see a current count even between writes
        // (e.g. a panic burst followed by a quiet stretch).
        while guard.front().is_some_and(|ts| *ts < cutoff_ms) {
            guard.pop_front();
        }
        u32::try_from(guard.len()).unwrap_or(u32::MAX)
    }

    /// Snapshot of the configured `max_concurrent_connections` ceiling.
    /// `0` until the accept loop calls [`Self::record_config`] for the
    /// first time — readers (`nagori doctor`, dashboards) interpret
    /// `0` as "daemon has not yet initialised", which is honest about
    /// the race between startup and the first probe.
    #[must_use]
    pub fn max_concurrent_connections(&self) -> usize {
        self.inner
            .max_concurrent_connections
            .load(Ordering::Relaxed)
    }

    /// Wire-format snapshot suitable for inclusion in `HealthResponse`
    /// and `DoctorReport`.
    #[must_use]
    pub fn report(&self) -> IpcHealthReport {
        IpcHealthReport {
            handler_panic_count: self.handler_panic_count(),
            last_panic_message: self.last_panic_message(),
            panics_last_5m: self.panics_last_5m(),
            max_concurrent_connections: u32::try_from(self.max_concurrent_connections())
                .unwrap_or(u32::MAX),
        }
    }

    /// Stamp the active [`IpcServerConfig`] onto the shared health
    /// snapshot. Called once by each accept loop before it starts
    /// accepting so `nagori doctor` / `nagori health` can show the
    /// active connection ceiling without an extra IPC roundtrip.
    pub fn record_config(&self, config: IpcServerConfig) {
        self.inner
            .max_concurrent_connections
            .store(config.max_concurrent_connections.get(), Ordering::Relaxed);
    }

    /// Convenience wrapper that redacts the caller's raw message before
    /// storing it. Test-only — the production accept loops use
    /// [`Self::record_redacted_panic`] so the same redacted string also
    /// reaches the `tracing` warn line, not just the health surface.
    /// Gated to the Unix test module that owns its only callers; without
    /// the `unix` bound it reads as dead code in the Windows test build.
    #[cfg(all(test, unix))]
    fn record_panic(&self, message: &str) {
        self.record_redacted_panic(redact_panic_message(message));
    }

    /// Variant for callers that have already redacted the panic payload
    /// (so the structured log surface and the health ring see exactly the
    /// same string). Avoids double-redaction work on the hot path and,
    /// more importantly, keeps a single source of truth for what an
    /// operator sees in logs vs. in `nagori doctor`.
    fn record_redacted_panic(&self, redacted: String) {
        let timestamp_ms = now_unix_ms();
        self.inner.handler_panics.fetch_add(1, Ordering::Relaxed);
        {
            let mut ring = match self.inner.panic_ring.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            if ring.len() == PANIC_RING_CAPACITY {
                ring.pop_front();
            }
            ring.push_back(PanicEntry { message: redacted });
        }
        let cutoff_ms = window_cutoff_ms();
        let mut window = match self.inner.panic_window.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        while window.front().is_some_and(|ts| *ts < cutoff_ms) {
            window.pop_front();
        }
        if window.len() == PANIC_WINDOW_MAX {
            // Saturation only kicks in under a runaway panic loop; drop
            // the oldest entry so the window keeps tracking the head of
            // the loop rather than freezing at the first burst.
            window.pop_front();
        }
        window.push_back(timestamp_ms);
    }

    /// Record that the accept loop just observed a new connection. Called
    /// from inside the per-platform accept loops so the daemon supervisor
    /// can distinguish a healthy-but-idle loop (probe drives a fresh bump)
    /// from a wedged loop (probe succeeds at the kernel level but the
    /// timestamp never advances).
    pub fn record_accept(&self) {
        self.inner
            .last_accept_at_ms
            .store(now_unix_ms(), Ordering::Relaxed);
    }

    /// Wall-clock millis-since-epoch of the most recent successful accept.
    /// Zero means the loop has not accepted anything yet — the supervisor
    /// seeds an initial value at spawn time so this is only ever `0`
    /// before the first `record_accept` lands.
    #[must_use]
    pub fn last_accept_at_ms(&self) -> u64 {
        self.inner.last_accept_at_ms.load(Ordering::Relaxed)
    }
}

/// Mask high-entropy hex runs and absolute user-home paths.
///
/// Panic payloads can quote auth tokens, content hashes, or filesystem
/// paths that reveal the operator's username or private directory layout.
/// The health surface is read by `nagori doctor` and `nagori health`,
/// both of which can land in logs and dashboards outside the daemon's
/// trust boundary; the two passes below give a cheap defence against
/// accidental leakage without depending on a regex crate just for this
/// one redactor. Source-file paths (`crates/.../foo.rs:123`, relative
/// `src/...`) are intentionally preserved so triage still has the call
/// site — only paths anchored at a known home-directory prefix are
/// scrubbed.
///
/// Pass ordering matters: home-path redaction runs first so that a long
/// hex path component inside a home path (`/Users/alice/.cache/<sha>/...`)
/// is consumed as part of the path, instead of being rewritten to
/// `<redacted-hex>` and then leaking the trailing components because the
/// synthesized `<` would be treated as a path terminator on the second
/// pass.
fn redact_panic_message(input: &str) -> String {
    redact_hex_runs(&redact_home_paths(input))
}

fn redact_hex_runs(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut hex_run = String::new();
    for ch in input.chars() {
        if ch.is_ascii_hexdigit() {
            hex_run.push(ch);
        } else {
            flush_hex_run(&mut out, &mut hex_run);
            out.push(ch);
        }
    }
    flush_hex_run(&mut out, &mut hex_run);
    out
}

fn flush_hex_run(out: &mut String, run: &mut String) {
    if run.len() >= 32 {
        out.push_str("<redacted-hex>");
    } else {
        out.push_str(run);
    }
    run.clear();
}

/// Known absolute-path prefixes that reveal the operator's username.
/// Each entry is matched case-insensitively against the ASCII bytes that
/// follow the prefix's anchor; the trailing path is then scrubbed until a
/// terminator is reached. The Windows prefix appears twice on purpose:
/// `tokio::task::JoinError::to_string()` forwards the panic payload as
/// rendered by its `Display`, which on `panic!("{:?}", path)` (the most
/// common `PathBuf` panic shape) escapes the backslashes into `\\`. The
/// single-backslash form covers `path.display()`-style panic messages,
/// and the double-backslash form covers `Debug`-formatted ones. Without
/// the doubled variant a `Debug`-formatted Windows panic would leak the
/// home segment verbatim. Note: longer prefixes must come first so the
/// linear `find` in `home_prefix_len` picks the most specific match.
const HOME_PATH_PREFIXES: &[&str] = &["C:\\\\Users\\\\", "C:\\Users\\", "/Users/", "/home/"];

fn redact_home_paths(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while !rest.is_empty() {
        if let Some(prefix_len) = home_prefix_len(rest) {
            out.push_str("<redacted-path>");
            let after_prefix = &rest[prefix_len..];
            let consumed = after_prefix
                .find(is_path_terminator)
                .unwrap_or(after_prefix.len());
            rest = &after_prefix[consumed..];
        } else {
            // `chars().next()` is `Some` while `rest` is non-empty; advance
            // by the UTF-8 width of the leading char so multi-byte chars
            // inside non-path prose survive intact.
            let ch = rest
                .chars()
                .next()
                .expect("rest is non-empty in this branch");
            out.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
    }
    out
}

fn home_prefix_len(input: &str) -> Option<usize> {
    HOME_PATH_PREFIXES
        .iter()
        .find(|prefix| {
            input.len() >= prefix.len()
                && input.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
        })
        .map(|prefix| prefix.len())
}

/// Path characters that mark the end of an absolute path in free-form
/// prose. Whitespace is *not* a terminator because macOS paths
/// legitimately embed spaces (`/Users/x/Library/Application Support/...`)
/// and a whitespace cut would leak the suffix. The trade-off is that a
/// bare path followed by prose (`/Users/x/foo crashed at 3`) over-redacts
/// the trailing prose, which is the safer failure mode for a privacy
/// surface. Newlines still terminate so multi-line panic backtraces
/// preserve their following frames. Quote / bracket / list-separator
/// characters cover paths embedded in `Debug` output, JSON-ish prose,
/// and quoted `Display` (`"/Users/x/Library/Application Support/y"`,
/// `Err(/Users/x/y)`, `path=/Users/x/y, fd=3`).
const fn is_path_terminator(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r'
            | '\t'
            | '\''
            | '"'
            | '`'
            | '('
            | ')'
            | '['
            | ']'
            | '{'
            | '}'
            | '<'
            | '>'
            | ','
            | ';'
    )
}

/// Best-effort UNIX millis-since-epoch. A pre-1970 system clock collapses
/// to `0`, which the supervisor treats as "no accept observed yet" — the
/// same fallback we use before the first accept actually fires.
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Earliest timestamp still considered "inside" [`PANICS_WINDOW`]. Older
/// entries should be dropped from `panic_window` before reading its length.
fn window_cutoff_ms() -> u64 {
    now_unix_ms().saturating_sub(u64::try_from(PANICS_WINDOW.as_millis()).unwrap_or(u64::MAX))
}

/// Inspect a reaped `JoinSet` result and route panics to `health`
/// (with a structured warn) while still surfacing non-panic join errors.
///
/// `abort()` during the drain stage generates `is_cancelled()` errors —
/// those are intentional and skipped here so a graceful shutdown does
/// not inflate the panic counter.
#[cfg(any(unix, windows))]
fn observe_handler_outcome(
    health: &IpcServerHealth,
    result: std::result::Result<(), tokio::task::JoinError>,
) {
    match result {
        Ok(()) => {}
        Err(err) if err.is_panic() => {
            // Redact once and reuse for both the structured log line and
            // the health surface. The `warn!` lands in `tracing`'s output,
            // which downstream pipelines may forward to log dashboards
            // outside the daemon's trust boundary; emitting the verbatim
            // join-error message there would defeat the redactor that
            // `record_panic` already applies on the health-report path.
            let redacted = redact_panic_message(&err.to_string());
            warn!(error = %redacted, "ipc_handler_panicked");
            health.record_redacted_panic(redacted);
        }
        Err(err) if err.is_cancelled() => {
            // Drain-stage `abort_all` is intentional; nothing to surface.
        }
        Err(err) => {
            warn!(error = %err, "ipc_handler_join_failed");
        }
    }
}

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

/// Hard ceiling on how long a single connection can block while the
/// response is written back. The read side is already bounded by
/// `READ_TIMEOUT`, but `write_all` + `flush` block once the transport's
/// socket / pipe buffer fills — so a client that authenticates, triggers a
/// large response, then stops reading would otherwise pin its connection
/// permit (one of the 32) and its handler task indefinitely. Thirty-two
/// such slow-readers would starve the legitimate CLI. Sized in the same
/// few-seconds band as `READ_TIMEOUT`: ample for the slowest realistic
/// local writeback, tight enough that a wedged reader frees its permit
/// promptly.
#[cfg(any(unix, windows))]
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

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
#[cfg(unix)]
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
                    Err(err) => break Err(AppError::Platform(err.to_string())),
                };
                // Bump the liveness timestamp before we touch the
                // semaphore. The supervisor's wedge probe relies on this
                // landing per accept; running it before the permit await
                // means even a saturated handler pool keeps the timestamp
                // advancing as long as accept() itself is still firing.
                server_health.record_accept();
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
            // bound for the lifetime of the daemon. Route the result through
            // `observe_handler_outcome` so a panicking handler is logged and
            // counted in `IpcServerHealth` instead of being silently dropped.
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                observe_handler_outcome(&server_health, result);
            }
        }
    };

    // Stage 1: drop the listener so no further `accept()` succeeds even
    // for clients that beat the shutdown signal in.
    drop(listener);

    // Stage 2: wait up to `drain_grace` for in-flight handlers to commit.
    if !tasks.is_empty() {
        let drain = async {
            while let Some(result) = tasks.join_next().await {
                observe_handler_outcome(&server_health, result);
            }
        };
        if timeout(drain_grace, drain).await.is_err() {
            // Stage 3: anything still running has had its grace period;
            // abort and reap so the JoinSet drops cleanly.
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
        // Bound the write-back the same way the read side is bounded. Once
        // the transport buffer fills, `write_all` blocks on a client that
        // has stopped reading; without a ceiling that handler — and the
        // connection permit it holds — would be pinned forever. On timeout
        // we fall through and return, dropping `stream` (closing the
        // connection) and `_permit` (freeing the slot) so a starved CLI can
        // make progress. Inner write errors stay best-effort, as before.
        let write_back = async {
            stream.write_all(&payload).await?;
            stream.write_all(b"\n").await?;
            // Best-effort flush so the client receives the response promptly
            // even on transports (named pipes) that buffer until shutdown.
            stream.flush().await
        };
        if timeout(WRITE_TIMEOUT, write_back).await.is_err() {
            warn!(
                timeout_ms = u64::try_from(WRITE_TIMEOUT.as_millis()).unwrap_or(u64::MAX),
                "ipc_write_timeout_dropping_slow_reader",
            );
        }
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

/// `serve_unix` variant that threads through a caller-supplied
/// `IpcServerHealth` so per-connection handler panics are counted and
/// surfaced via `nagori health` / `nagori doctor`.
#[cfg(unix)]
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
    let listener = bind_unix(path).await?;
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
/// Authentication is enforced both by an explicit DACL on the pipe
/// (current-user SID only — see [`pipe_security_handle`]) and by the
/// sibling token file. The token file ACL similarly restricts read access
/// to the current user, BUILTIN\Administrators, and NT AUTHORITY\SYSTEM.
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
    // callers are additionally restricted by the explicit DACL applied
    // through `create_with_security_attributes_raw` below.
    opts.reject_remote_clients(true);
    opts
}

/// Build a [`SecurityHandle`] suitable for a Windows named-pipe server:
/// DACL with a single ACE that grants the current user `GENERIC_READ |
/// GENERIC_WRITE` (and nothing to anyone else, including other local users
/// on the same desktop session).
#[cfg(windows)]
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
#[cfg(windows)]
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
#[cfg(windows)]
pub fn bind_pipe(pipe_name: &str) -> Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    create_pipe_instance(pipe_name, true)
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
        IpcServerHealth::default(),
        IpcServerConfig::default(),
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

    #[test]
    fn panics_last_5m_counts_past_ring_capacity() {
        // The message ring caps at PANIC_RING_CAPACITY (8); the timestamp
        // window must keep growing past it so the 5-minute rate reflects
        // a real panic loop rather than saturating at the ring's limit.
        let health = IpcServerHealth::new();
        for i in 0..(PANIC_RING_CAPACITY + 5) {
            health.record_panic(&format!("boom {i}"));
        }
        assert_eq!(
            u32::try_from(PANIC_RING_CAPACITY + 5).expect("usize fits u32"),
            health.panics_last_5m()
        );
        // The message ring stays at its cap and exposes the latest.
        assert_eq!(
            health.last_panic_message().as_deref(),
            Some(format!("boom {}", PANIC_RING_CAPACITY + 4).as_str()),
        );
    }

    #[test]
    fn record_panic_redacts_long_hex_runs() {
        let health = IpcServerHealth::new();
        // 32+ ascii-hex run should be masked; "boom" survives verbatim.
        health.record_panic(
            "boom: token=deadbeefcafebabe1234567890abcdef0fedcba98765432100ff at row 7",
        );
        let last = health
            .last_panic_message()
            .expect("a panic was just recorded");
        assert!(
            last.contains("<redacted-hex>"),
            "redactor should mask the long hex run: {last}",
        );
        assert!(
            last.contains("boom"),
            "non-hex prose should survive: {last}"
        );
    }

    #[test]
    fn record_panic_redacts_user_home_paths() {
        // Paths anchored under a known home prefix leak the operator's
        // username when the panic surface is shipped to logs / dashboards;
        // each platform's home-dir convention must scrub. Source-file
        // paths (no home prefix) are kept verbatim so triage still has
        // the call site to look at.
        let cases = [
            "panicked at /Users/alice/Library/secret reading row 5",
            "open '/home/bob/.config/nagori/state'",
            r"open C:\Users\carol\AppData\Roaming\nagori for write",
            "lower-case: open /users/alice/foo",
        ];
        for case in cases {
            let health = IpcServerHealth::new();
            health.record_panic(case);
            let last = health
                .last_panic_message()
                .expect("a panic was just recorded");
            assert!(
                last.contains("<redacted-path>"),
                "redactor should mask the home path: input={case:?} output={last:?}",
            );
            for needle in ["alice", "bob", "carol"] {
                assert!(
                    !last.contains(needle),
                    "redactor should strip the username component: {last:?}",
                );
            }
        }
        // Source-file references must survive so a `nagori doctor` reader
        // can correlate panics with code.
        let health = IpcServerHealth::new();
        health.record_panic("panicked at crates/nagori-ipc/src/server.rs:42:13");
        let last = health
            .last_panic_message()
            .expect("a panic was just recorded");
        assert!(
            last.contains("crates/nagori-ipc/src/server.rs"),
            "relative source paths must survive: {last:?}",
        );
    }

    #[test]
    fn record_panic_redacts_paths_containing_long_hex_components() {
        // Cache-style paths often embed a content hash as a directory
        // component. If the hex pass ran first it would rewrite the
        // component to `<redacted-hex>`; the synthesised `<` would then
        // act as a path terminator on the home-path pass and leak the
        // suffix (`/private/file`). The redactor runs home-path first
        // specifically to defend against that ordering bug — pin it
        // here so a future refactor that swaps the passes regresses
        // loudly.
        let health = IpcServerHealth::new();
        health.record_panic(
            "open /Users/alice/.cache/deadbeefcafebabe1234567890abcdef0fedcba98765432100ff/private/file failed",
        );
        let last = health
            .last_panic_message()
            .expect("a panic was just recorded");
        assert!(
            !last.contains("/private/file"),
            "redactor must consume the suffix past the hex component: {last:?}",
        );
        assert!(
            !last.contains("alice"),
            "redactor must strip the username component: {last:?}",
        );
        assert!(
            last.contains("<redacted-path>"),
            "redactor should leave the home-path marker: {last:?}",
        );
    }

    #[test]
    fn record_panic_redacts_debug_formatted_windows_paths() {
        // `panic!("{:?}", path)` is the idiomatic shape for printing a
        // `PathBuf` in a panic message, and its Display-via-Debug escapes
        // every backslash. The redactor must therefore recognise both
        // `C:\Users\bob` (single-backslash, from `path.display()`) and
        // `C:\\Users\\bob` (double-backslash, from Debug). Without the
        // double-backslash prefix the operator's Windows username would
        // leak verbatim into `nagori doctor`.
        let health = IpcServerHealth::new();
        health.record_panic(r#"open "C:\\Users\\bob\\AppData\\Roaming\\nagori" failed"#);
        let last = health
            .last_panic_message()
            .expect("a panic was just recorded");
        assert!(
            !last.contains("bob"),
            "Debug-escaped Windows path must be redacted: {last:?}",
        );
        assert!(
            last.contains("<redacted-path>"),
            "redactor should leave the home-path marker: {last:?}",
        );
        assert!(
            last.contains("failed"),
            "post-closing-quote prose must survive: {last:?}",
        );
    }

    #[test]
    fn record_panic_redacts_paths_with_embedded_spaces() {
        // macOS path components legitimately contain spaces (`Application
        // Support`, `Mobile Documents`), so a redactor that stops at the
        // first whitespace would leak the suffix and defeat the goal of
        // hiding any post-home component. Quoted forms must terminate at
        // the closing quote so prose after the path survives intact.
        let health = IpcServerHealth::new();
        health.record_panic(
            r#"open "/Users/alice/Library/Application Support/nagori/state" failed: ENOENT"#,
        );
        let last = health
            .last_panic_message()
            .expect("a panic was just recorded");
        assert!(
            !last.contains("Application Support"),
            "redactor must consume the space-bearing suffix: {last:?}",
        );
        assert!(
            !last.contains("alice"),
            "redactor must strip the username: {last:?}",
        );
        assert!(
            last.contains("ENOENT"),
            "post-closing-quote prose must survive: {last:?}",
        );
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
        let listener = bind_unix(&path).await.expect("bind");
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
}

/// Transport-agnostic tests for the shared [`handle_connection`] driver.
///
/// Both the Unix-socket and named-pipe servers funnel through
/// `handle_connection`, so a `tokio::io::duplex` peer that authenticates
/// but never reads the response exercises the same slow-reader write path
/// for both. Compiled on every platform that has a server so the
/// regression runs in both the Unix-socket and named-pipe CI matrices.
#[cfg(all(test, any(unix, windows)))]
mod tests_transport {
    use std::sync::Arc;

    use super::*;
    use crate::IpcEnvelope;

    fn test_token() -> AuthToken {
        AuthToken::generate().expect("token should generate")
    }

    #[tokio::test(start_paused = true)]
    async fn slow_reader_releases_permit_after_write_timeout() {
        // Regression: before `WRITE_TIMEOUT`, the write-back path was a
        // bare `write_all` + `flush`. A client that authenticated, drew a
        // response, then stopped reading would fill the transport buffer
        // and block the handler forever — pinning one of the 32
        // connection permits. Thirty-two such peers would starve the
        // legitimate CLI. The handler below returns a response far larger
        // than the duplex buffer, the peer never reads it, and we assert
        // the connection times out and frees its permit.
        let token = Arc::new(test_token());
        let handler = Arc::new(|_request: IpcRequest| async {
            // ~1 KiB error response, well above the 16-byte duplex buffer
            // and well under `MAX_IPC_RESPONSE_BYTES` (1 MiB) so it is
            // written rather than rejected as oversized.
            IpcResponse::Error(crate::IpcError {
                code: "x".repeat(512),
                message: "y".repeat(512),
                recoverable: false,
            })
        });

        // A single permit models the production semaphore: a permit that
        // is never returned is exactly the starvation bug.
        let semaphore = Arc::new(Semaphore::new(1));
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("permit should be available");
        assert_eq!(semaphore.available_permits(), 0);

        let request = serde_json::to_vec(&IpcEnvelope {
            token: token.as_str().to_owned(),
            request: IpcRequest::Health,
        })
        .expect("serialise envelope");

        // Tight buffer so even the small response cannot drain in one
        // shot once the peer stops reading.
        let (server_io, mut client_io) = tokio::io::duplex(16);

        let start = tokio::time::Instant::now();
        let server = handle_connection(server_io, permit, handler, token);
        let client = async {
            client_io
                .write_all(&request)
                .await
                .expect("client should write the request envelope");
            client_io
                .write_all(b"\n")
                .await
                .expect("client should terminate the request line");
            // Deliberately never read the response, holding the
            // connection open so the server's write blocks on a full
            // buffer. `pending` parks without a timer so the only timer
            // left is the server's `WRITE_TIMEOUT`, which paused-time
            // auto-advance fires.
            std::future::pending::<()>().await;
        };

        tokio::select! {
            () = server => {}
            () = client => unreachable!("the slow reader never finishes on its own"),
        }

        let elapsed = start.elapsed();
        assert!(
            elapsed >= WRITE_TIMEOUT,
            "handle_connection must block until WRITE_TIMEOUT fires (not error out early): {elapsed:?}",
        );
        assert_eq!(
            semaphore.available_permits(),
            1,
            "the connection permit must be released once the slow reader times out",
        );
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
