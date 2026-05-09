use std::sync::{Arc, Mutex};

use nagori_ipc::MaintenanceHealthReport;

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
}
