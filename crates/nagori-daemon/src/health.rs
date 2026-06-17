use std::sync::Arc;

use nagori_ipc::{
    CaptureEventCategory, CaptureHealthReport, MaintenanceHealthReport, StartupHealthReport,
};
use parking_lot::{Mutex, MutexGuard};
use time::OffsetDateTime;

/// After this many consecutive failed maintenance runs the loop is
/// reported as `degraded` to clients (`nagori health`, `nagori doctor`).
///
/// 3 picks up the difference between a one-shot transient failure (which
/// `warn!` in the inner loop is enough to surface) and a sustained
/// outage (locked DB, FTS5 corruption, missing migrations) that the
/// operator needs to know about by polling rather than tailing logs.
pub const MAINTENANCE_DEGRADED_THRESHOLD: u32 = 3;

/// After this many consecutive `capture_once` errors the capture loop is
/// reported as `degraded` to clients (`nagori doctor`, desktop tray
/// tooltip).
///
/// Capture errors are noisier than maintenance failures (a single AX
/// flake or pasteboard read hiccup is normal), so the threshold sits at
/// the same value the loop's internal exponential backoff uses
/// (`BACKOFF_AFTER_CONSECUTIVE_FAILURES`) — once the loop itself has
/// decided the failure is sustained enough to slow polling down, the
/// health surface treats it as degraded too. Intentional drops
/// (oversized payload, policy / secret refusal) do *not* count against
/// this counter; they're recorded on `CaptureHealth` with a category
/// instead so the UI can distinguish "we're losing visibility" from
/// "we're rejecting on purpose".
pub const CAPTURE_DEGRADED_THRESHOLD: u32 = 3;

/// Shared health snapshot of the maintenance background loop.
///
/// Updated from `serve/lifecycle.rs` after each iteration and read by the IPC
/// `Health` and `Doctor` handlers. The lock is held briefly enough that
/// using a sync mutex over an async lock is fine (no awaits while held);
/// the alternative — an `AtomicU32` plus a separate string slot — would
/// split the consecutive-failures count from the matching error message
/// and risk reporting "degraded with no error" during the update window.
/// `parking_lot::Mutex` is used so a panic inside the critical section
/// (e.g. a future allocator failure inside `String::clone`) cannot leave
/// the health endpoint permanently broken — there is no poison state to
/// recover from.
#[derive(Debug, Default, Clone)]
pub struct MaintenanceHealth {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug, Default)]
struct Inner {
    consecutive_failures: u32,
    last_error: Option<String>,
}

impl MaintenanceHealth {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a successful maintenance run: clear the failure counter
    /// and the cached error message so the next snapshot reads clean.
    pub fn record_success(&self) {
        let mut guard = self.lock();
        guard.consecutive_failures = 0;
        guard.last_error = None;
    }

    /// Record a failed maintenance run, capturing the latest error
    /// message. Saturating add so a runaway failure mode (e.g. a
    /// permanently locked DB) eventually plateaus instead of wrapping.
    pub fn record_failure(&self, message: impl Into<String>) {
        let mut guard = self.lock();
        guard.consecutive_failures = guard.consecutive_failures.saturating_add(1);
        guard.last_error = Some(message.into());
    }

    /// Wire-format snapshot suitable for inclusion in `HealthResponse` /
    /// `DoctorReport`.
    pub fn report(&self) -> MaintenanceHealthReport {
        let guard = self.lock();
        MaintenanceHealthReport {
            consecutive_failures: guard.consecutive_failures,
            degraded: guard.consecutive_failures >= MAINTENANCE_DEGRADED_THRESHOLD,
            last_error: guard.last_error.clone(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // `parking_lot::Mutex` has no poison state, so a previous panic
        // inside the critical section cannot wedge later readers — every
        // caller gets a fresh guard. Health snapshots remain available
        // even after a transient panic during update.
        self.inner.lock()
    }
}

/// Shared one-shot health snapshot of the capture loop's initialisation.
///
/// The desktop's "ready" notification used to fire unconditionally right
/// after the background tasks were spawned, even when the capture task
/// silently aborted on `refresh_settings_from_store()` failure. This
/// surface records the first definitive outcome of that init step —
/// either `ready` once the settings are loaded and the capture loop is
/// entering its polling stage, or `failed` with the error message — so
/// `nagori doctor` and the desktop notification path can branch on a
/// real signal instead of guessing.
///
/// The lock is held briefly enough that using a sync mutex over an
/// async lock is fine (no awaits while held). `parking_lot::Mutex` is
/// used so a panic inside the critical section can't leave the health
/// endpoint permanently broken — there is no poison state to recover
/// from.
#[derive(Debug, Default, Clone)]
pub struct StartupHealth {
    inner: Arc<Mutex<StartupInner>>,
}

#[derive(Debug, Default)]
struct StartupInner {
    /// `None` until the capture task posts its outcome, then either
    /// `Some(Ok(()))` (capture loop entered polling) or `Some(Err(msg))`
    /// (settings load aborted before polling started).
    outcome: Option<std::result::Result<(), String>>,
}

impl StartupHealth {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that the capture loop's pre-poll initialisation completed
    /// successfully. Idempotent — only the first call lands so a later
    /// settings reload or compositor re-init can't accidentally clear a
    /// previously recorded failure.
    pub fn record_capture_ready(&self) {
        let mut guard = self.lock();
        if guard.outcome.is_none() {
            guard.outcome = Some(Ok(()));
        }
    }

    /// Record that the capture loop aborted during initialisation. Like
    /// `record_capture_ready`, idempotent on the first call so the very
    /// first failure (typically the settings-load abort path) survives
    /// any subsequent retries.
    pub fn record_capture_failed(&self, message: impl Into<String>) {
        let mut guard = self.lock();
        if guard.outcome.is_none() {
            guard.outcome = Some(Err(message.into()));
        }
    }

    /// Wire-format snapshot suitable for `DoctorReport`.
    ///
    /// `ready` is `false` until the capture task posts its outcome, and
    /// stays `false` if it posts a failure. `last_error` carries the
    /// recorded message on the failure path.
    pub fn report(&self) -> StartupHealthReport {
        let guard = self.lock();
        match guard.outcome.as_ref() {
            Some(Ok(())) => StartupHealthReport {
                ready: true,
                last_error: None,
            },
            Some(Err(message)) => StartupHealthReport {
                ready: false,
                last_error: Some(message.clone()),
            },
            None => StartupHealthReport {
                ready: false,
                last_error: None,
            },
        }
    }

    fn lock(&self) -> MutexGuard<'_, StartupInner> {
        self.inner.lock()
    }
}

/// Shared health snapshot of the capture loop's per-tick outcomes.
///
/// The maintenance and startup surfaces above cover periodic retention
/// runs and the one-shot pre-poll init. Neither catches the steady-state
/// failure mode this struct exists to surface: a capture loop that has
/// entered polling but is silently dropping every clip — either because the
/// adapter keeps erroring (revoked permissions, wedged `AppKit`) or
/// because the user's `max_entry_size_bytes` / `regex_denylist` /
/// `secret_handling=block` settings reject every observed sequence.
/// Without a category, `nagori doctor` and the desktop tray can't
/// distinguish "we lost visibility" from "we're filtering on purpose",
/// and the user perceives both as "nothing gets saved".
///
/// `record_success(at)` updates the last-success anchor and resets the
/// failure counter. `record_error(category, message, at)` bumps the
/// counter and stores the latest error + category. `record_drop(
/// category, at)` records an intentional drop without incrementing the
/// failure counter — the category is still updated so the UI can flag
/// "the loop is running but everything is being filtered out". The
/// failure counter and the drop category are tracked separately so a
/// burst of policy drops can't shadow a real adapter outage.
///
/// As with the other health surfaces, the lock is held briefly enough
/// that a sync mutex over an async lock is fine (no awaits while held),
/// and `parking_lot::Mutex` is used so a panic inside the critical
/// section cannot leave the health endpoint permanently broken — there
/// is no poison state to recover from.
#[derive(Debug, Default, Clone)]
pub struct CaptureHealth {
    inner: Arc<Mutex<CaptureInner>>,
}

#[derive(Debug, Default)]
struct CaptureInner {
    last_success_at: Option<OffsetDateTime>,
    consecutive_failures: u32,
    last_error: Option<String>,
    last_event_category: Option<CaptureEventCategory>,
    last_event_at: Option<OffsetDateTime>,
}

impl CaptureHealth {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that a `capture_once` tick produced or skipped a clip
    /// without erroring. Anchors `last_success_at` so the UI can render
    /// "last captured 3 minutes ago" without the loop having to plumb
    /// its own clock, and clears the consecutive-failures counter so a
    /// recovered adapter immediately reads `degraded=false`.
    ///
    /// The drop category is intentionally *not* cleared: a steady run
    /// of successful captures should not erase the fact that the most
    /// recent observable event was, say, an oversized drop — readers
    /// only care about "what was the most recent non-success outcome".
    pub fn record_success(&self, at: OffsetDateTime) {
        let mut guard = self.lock();
        guard.last_success_at = Some(at);
        guard.consecutive_failures = 0;
        guard.last_error = None;
    }

    /// Record an error-class capture outcome (adapter failure,
    /// settings-load error). Bumps the consecutive-failures counter,
    /// captures the error message, and records the category + observed
    /// time so the UI can show *why* the capture loop is degraded.
    pub fn record_error(
        &self,
        category: CaptureEventCategory,
        message: impl Into<String>,
        at: OffsetDateTime,
    ) {
        let mut guard = self.lock();
        guard.consecutive_failures = guard.consecutive_failures.saturating_add(1);
        guard.last_error = Some(message.into());
        guard.last_event_category = Some(category);
        guard.last_event_at = Some(at);
    }

    /// Record an intentional drop (oversized payload, policy / secret
    /// refusal). The failure counter is *not* incremented because the
    /// loop did its job — but the category + timestamp are preserved so
    /// the UI can flag "every recent clip was filtered out".
    pub fn record_drop(&self, category: CaptureEventCategory, at: OffsetDateTime) {
        let mut guard = self.lock();
        guard.last_event_category = Some(category);
        guard.last_event_at = Some(at);
    }

    /// Wire-format snapshot suitable for inclusion in `HealthResponse` /
    /// `DoctorReport`.
    pub fn report(&self) -> CaptureHealthReport {
        let guard = self.lock();
        CaptureHealthReport {
            last_success_at: guard.last_success_at,
            consecutive_failures: guard.consecutive_failures,
            degraded: guard.consecutive_failures >= CAPTURE_DEGRADED_THRESHOLD,
            last_error: guard.last_error.clone(),
            last_event_category: guard.last_event_category,
            last_event_at: guard.last_event_at,
        }
    }

    fn lock(&self) -> MutexGuard<'_, CaptureInner> {
        self.inner.lock()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_failure_increments_counter_and_captures_message() {
        let health = MaintenanceHealth::new();
        health.record_failure("disk full");
        let report = health.report();
        assert_eq!(report.consecutive_failures, 1);
        assert!(!report.degraded);
        assert_eq!(report.last_error.as_deref(), Some("disk full"));
    }

    #[test]
    fn report_marks_degraded_at_threshold() {
        let health = MaintenanceHealth::new();
        for _ in 0..MAINTENANCE_DEGRADED_THRESHOLD {
            health.record_failure("locked");
        }
        let report = health.report();
        assert_eq!(report.consecutive_failures, MAINTENANCE_DEGRADED_THRESHOLD);
        assert!(report.degraded);
    }

    #[test]
    fn record_success_clears_failure_state() {
        let health = MaintenanceHealth::new();
        health.record_failure("first");
        health.record_failure("second");
        health.record_success();
        let report = health.report();
        assert_eq!(report.consecutive_failures, 0);
        assert!(!report.degraded);
        assert!(report.last_error.is_none());
    }

    #[test]
    fn startup_health_defaults_to_not_ready() {
        let health = StartupHealth::new();
        let report = health.report();
        assert!(!report.ready);
        assert!(report.last_error.is_none());
    }

    #[test]
    fn startup_health_records_capture_ready() {
        let health = StartupHealth::new();
        health.record_capture_ready();
        let report = health.report();
        assert!(report.ready);
        assert!(report.last_error.is_none());
    }

    #[test]
    fn startup_health_records_capture_failed_with_message() {
        let health = StartupHealth::new();
        health.record_capture_failed("settings load aborted");
        let report = health.report();
        assert!(!report.ready);
        assert_eq!(report.last_error.as_deref(), Some("settings load aborted"));
    }

    #[test]
    fn capture_health_record_error_increments_and_categorises() {
        let health = CaptureHealth::new();
        let at = OffsetDateTime::now_utc();
        health.record_error(CaptureEventCategory::Adapter, "ax read failed", at);
        let report = health.report();
        assert_eq!(report.consecutive_failures, 1);
        assert!(!report.degraded);
        assert_eq!(report.last_error.as_deref(), Some("ax read failed"));
        assert_eq!(
            report.last_event_category,
            Some(CaptureEventCategory::Adapter)
        );
        assert_eq!(report.last_event_at, Some(at));
    }

    #[test]
    fn capture_health_marks_degraded_at_threshold() {
        let health = CaptureHealth::new();
        for _ in 0..CAPTURE_DEGRADED_THRESHOLD {
            health.record_error(
                CaptureEventCategory::Adapter,
                "flake",
                OffsetDateTime::now_utc(),
            );
        }
        let report = health.report();
        assert_eq!(report.consecutive_failures, CAPTURE_DEGRADED_THRESHOLD);
        assert!(report.degraded);
    }

    #[test]
    fn capture_health_record_drop_preserves_failure_counter() {
        // Intentional drops (policy / oversized) must not bump the failure
        // counter — the loop did its job. The drop category is still
        // recorded so the UI can distinguish "we lost visibility" from
        // "we're rejecting on purpose" once both have happened.
        let health = CaptureHealth::new();
        health.record_error(
            CaptureEventCategory::Adapter,
            "lost",
            OffsetDateTime::now_utc(),
        );
        let at = OffsetDateTime::now_utc();
        health.record_drop(CaptureEventCategory::OversizedDrop, at);
        let report = health.report();
        assert_eq!(report.consecutive_failures, 1);
        assert_eq!(
            report.last_event_category,
            Some(CaptureEventCategory::OversizedDrop)
        );
        assert_eq!(report.last_event_at, Some(at));
        // The latest error message is preserved across drops so the UI
        // can keep showing the underlying adapter outage.
        assert_eq!(report.last_error.as_deref(), Some("lost"));
    }

    #[test]
    fn capture_health_record_success_clears_errors_keeps_drop_category() {
        // Successful captures reset the failure counter and the cached
        // error message — but the drop category is preserved so a string
        // of successes after an oversized drop still surfaces "the most
        // recent non-success was an oversized payload".
        let health = CaptureHealth::new();
        health.record_drop(
            CaptureEventCategory::OversizedDrop,
            OffsetDateTime::now_utc(),
        );
        health.record_error(
            CaptureEventCategory::Adapter,
            "transient",
            OffsetDateTime::now_utc(),
        );
        let success_at = OffsetDateTime::now_utc();
        health.record_success(success_at);
        let report = health.report();
        assert_eq!(report.consecutive_failures, 0);
        assert!(!report.degraded);
        assert!(report.last_error.is_none());
        assert_eq!(report.last_success_at, Some(success_at));
        // Drop / error category is preserved across the success — readers
        // only care about the most recent non-success outcome.
        assert_eq!(
            report.last_event_category,
            Some(CaptureEventCategory::Adapter)
        );
    }

    #[test]
    fn startup_health_first_outcome_wins() {
        // The first definitive outcome must be sticky: a later retry
        // that records "ready" cannot mask the initial failure (and vice
        // versa). Otherwise a transient capture-loop restart could
        // silently flip `nagori doctor` back to `ready=true` while the
        // user is staring at the failure message in the UI.
        let health = StartupHealth::new();
        health.record_capture_failed("first error");
        health.record_capture_ready();
        let report = health.report();
        assert!(!report.ready);
        assert_eq!(report.last_error.as_deref(), Some("first error"));

        let health = StartupHealth::new();
        health.record_capture_ready();
        health.record_capture_failed("later");
        let report = health.report();
        assert!(report.ready);
        assert!(report.last_error.is_none());
    }
}
