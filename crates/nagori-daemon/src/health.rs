use std::sync::{Arc, Mutex};

use nagori_ipc::{MaintenanceHealthReport, StartupHealthReport};

/// After this many consecutive failed maintenance runs the loop is
/// reported as `degraded` to clients (`nagori health`, `nagori doctor`).
///
/// 3 picks up the difference between a one-shot transient failure (which
/// `warn!` in the inner loop is enough to surface) and a sustained
/// outage (locked DB, FTS5 corruption, missing migrations) that the
/// operator needs to know about by polling rather than tailing logs.
pub const MAINTENANCE_DEGRADED_THRESHOLD: u32 = 3;

/// Shared health snapshot of the maintenance background loop.
///
/// Updated from `serve.rs` after each iteration and read by the IPC
/// `Health` and `Doctor` handlers. The lock is held briefly enough that
/// using `std::sync::Mutex` over an async lock is fine (no awaits while
/// held); the alternative — an `AtomicU32` plus a separate string slot —
/// would split the consecutive-failures count from the matching error
/// message and risk reporting "degraded with no error" during the
/// update window.
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

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // PoisonError carries the last value, so we can recover and keep
        // serving health snapshots even if a previous holder panicked.
        // Doing anything else here would convert "we crashed once" into
        // "the daemon's health endpoint is now permanently broken".
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
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
/// The lock is held briefly enough that using `std::sync::Mutex` over an
/// async lock is fine (no awaits while held). A poisoned mutex is
/// recovered the same way `MaintenanceHealth` recovers — a previous
/// panic must not permanently break the health endpoint.
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

    fn lock(&self) -> std::sync::MutexGuard<'_, StartupInner> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
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
