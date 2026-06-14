//! IPC server tuning and health observability.
//!
//! Holds [`IpcServerConfig`] (the per-connection concurrency ceiling) and
//! [`IpcServerHealth`] (the cloneable observer the accept loops thread
//! through so handler panics surface in `nagori doctor` / `nagori health`),
//! plus the panic-message redactor that keeps tokens and home paths off the
//! health surface.

use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[cfg(any(unix, windows))]
use tracing::warn;

use crate::IpcHealthReport;

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

#[derive(Debug)]
struct IpcServerHealthInner {
    handler_panics: AtomicU64,
    /// Monotonic millis-since-[`clock_origin`](Self::clock_origin) of the most
    /// recent `listener.accept()` (Unix) / `NamedPipeServer::connect()`
    /// (Windows) completion, stored as `elapsed_ms + 1` so a genuine record is
    /// always non-zero and `0` unambiguously means "no accept observed yet".
    /// The daemon's supervisor reads the derived freshness via
    /// [`IpcServerHealth::accept_age`] — combined with periodic self-probes —
    /// to detect an accept loop that wedged on the OS side (handler deadlock,
    /// kernel-level resource exhaustion) without exiting the spawned task.
    /// Daemon liveness alone would miss that class of failure because the
    /// supervisor only respawns on task exit, not on silent input starvation.
    ///
    /// A monotonic base (rather than wall-clock) is deliberate: an NTP step or
    /// manual clock change must not make a healthy loop look wedged (forward
    /// jump) or mask a real wedge (backward jump). Both this and the panic
    /// window are process-internal durations, so `Instant` is the correct
    /// clock.
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
    /// Timestamps (monotonic millis-since-[`clock_origin`](Self::clock_origin))
    /// of every panic observed in the last [`PANICS_WINDOW`], capped at
    /// [`PANIC_WINDOW_MAX`]. Pruned by timestamp on every push and every read
    /// so the deque size tracks the active panic rate. Separate from
    /// `panic_ring` so a tight panic loop with more than [`PANIC_RING_CAPACITY`]
    /// hits inside the window doesn't get under-reported by `panics_last_5m`.
    panic_window: Mutex<VecDeque<u64>>,
    /// Monotonic reference instant captured at construction. Every timestamp
    /// the inner stores (`last_accept_at_ms`, `panic_window`) is measured as
    /// elapsed millis from here, so all freshness/window arithmetic is immune
    /// to wall-clock steps.
    clock_origin: Instant,
}

impl IpcServerHealthInner {
    /// Millis elapsed since [`Self::clock_origin`], saturating at `u64::MAX`
    /// (reached only after ~584 million years of uptime).
    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.clock_origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Earliest `elapsed_ms` timestamp still inside [`PANICS_WINDOW`]. Older
    /// `panic_window` entries are dropped before its length is read.
    fn window_cutoff_ms(&self) -> u64 {
        self.elapsed_ms()
            .saturating_sub(u64::try_from(PANICS_WINDOW.as_millis()).unwrap_or(u64::MAX))
    }
}

impl IpcServerHealth {
    #[must_use]
    pub fn new() -> Self {
        Self {
            // Constructed explicitly (not via `Default`) because the monotonic
            // `clock_origin` must be stamped with `Instant::now()` at creation.
            inner: Arc::new(IpcServerHealthInner {
                handler_panics: AtomicU64::new(0),
                last_accept_at_ms: AtomicU64::new(0),
                max_concurrent_connections: AtomicUsize::new(0),
                panic_ring: Mutex::new(VecDeque::new()),
                panic_window: Mutex::new(VecDeque::new()),
                clock_origin: Instant::now(),
            }),
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
        let cutoff_ms = self.inner.window_cutoff_ms();
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
        let timestamp_ms = self.inner.elapsed_ms();
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
        let cutoff_ms = self.inner.window_cutoff_ms();
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
        // Store `elapsed + 1` so a genuine record is always non-zero and `0`
        // stays reserved for "never accepted" (see `accept_age`).
        self.inner
            .last_accept_at_ms
            .store(self.inner.elapsed_ms().saturating_add(1), Ordering::Relaxed);
    }

    /// How long ago the most recent accept landed, on the monotonic clock.
    /// `None` means no accept has been observed yet — the accept loop seeds a
    /// value before its first await, so this is only `None` in the narrow
    /// window before that seed lands. Immune to wall-clock steps, so the
    /// supervisor's wedge detector can't be tripped by an NTP jump.
    #[must_use]
    pub fn accept_age(&self) -> Option<Duration> {
        let stored = self.inner.last_accept_at_ms.load(Ordering::Relaxed);
        if stored == 0 {
            return None;
        }
        // `stored` is `recorded_elapsed_ms + 1`; recover the original and
        // subtract from the current elapsed. `saturating_sub` guards the
        // (impossible on a monotonic clock, but cheap) case of a stale read.
        let recorded_ms = stored - 1;
        let age_ms = self.inner.elapsed_ms().saturating_sub(recorded_ms);
        Some(Duration::from_millis(age_ms))
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

/// Inspect a reaped `JoinSet` result and route panics to `health`
/// (with a structured warn) while still surfacing non-panic join errors.
///
/// `abort()` during the drain stage generates `is_cancelled()` errors —
/// those are intentional and skipped here so a graceful shutdown does
/// not inflate the panic counter.
#[cfg(any(unix, windows))]
pub(super) fn observe_handler_outcome(
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn accept_age_is_none_before_first_accept_then_recent() {
        // The supervisor relies on `None` meaning "no accept seen yet" (skip
        // the wedge check) and a small `Some` meaning "fresh". A genuine record
        // must never collapse to the `None` sentinel even when it lands at
        // ~0ms elapsed — that's what the `+1` store offset guards.
        let health = IpcServerHealth::new();
        assert!(
            health.accept_age().is_none(),
            "no accept observed yet must read as None"
        );
        health.record_accept();
        let age = health
            .accept_age()
            .expect("an accept was just recorded, so age must be Some");
        // Measured on the monotonic clock, so it is a small real duration —
        // not a wall-clock epoch. Can't pin an exact value, but it must be
        // recent and well under the 90s wedge threshold.
        assert!(
            age < Duration::from_secs(5),
            "a freshly recorded accept should read as recent: {age:?}"
        );
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
}
