use crate::state::AppState;
use tauri::Manager;

/// Hard cap on how long the startup notification waits for the capture
/// loop to report its outcome. Picked at 10 s so a slow first
/// `refresh_settings_from_store` (cold `SQLite` open on a large history)
/// still gets a tailored message, but a wedged init doesn't leave the
/// user without any feedback. After the cap the notification falls back
/// to the neutral "Nagori started" body — `nagori doctor` continues to
/// report the eventual outcome once it lands.
const STARTUP_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long to wait between `StartupHealth` polls. The snapshot is
/// updated once during init and is cheap to read (one mutex lock), so a
/// short interval keeps the perceived latency low without measurably
/// burning CPU.
const STARTUP_READY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// After startup reports ready, how long to give the capture loop to
/// either land a successful tick or accumulate enough failures to flip
/// `degraded` before resolving the notification body.
///
/// `StartupHealth.ready` flips the moment `refresh_settings_from_store`
/// returns successfully — that happens before the very first polling
/// tick fires, so a bare `ready` snapshot tells us nothing about
/// whether the loop is actually going to be able to capture clips.
/// Without this settle window we'd unconditionally send "Nagori is
/// running" even while the first three ticks are all about to error,
/// resurrecting the silent-data-loss bug the gate is supposed to catch.
///
/// 2 s comfortably covers four ticks at the default 500 ms cadence —
/// the loop needs three consecutive failures to flip `degraded` (≈1.5
/// s), so a true outage is observed before this elapses. If the user
/// configured a much slower cadence and no tick has fired yet, we fall
/// back to `Ready`: nagori doctor remains the source of truth once an
/// outcome lands.
const CAPTURE_READY_SETTLE_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

/// Outcome we render into the OS notification. Split out so the body
/// selection is unit-testable without spinning up a Tauri runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupNotice {
    /// Capture loop entered polling. The notification body confirms
    /// readiness without lying about what's running underneath.
    Ready,
    /// Capture loop entered polling but already crossed the degraded
    /// threshold inside the startup window (adapter errors,
    /// settings-load failures). The notification body has to be honest
    /// about that — claiming "ready" while every tick is silently
    /// failing is the original silent-data-loss bug `CaptureHealth`
    /// exists to surface.
    ReadyButDegraded,
    /// Capture loop aborted (typically settings-load failure). The body
    /// directs the user to `nagori doctor` for the recorded reason.
    Failed,
    /// Init did not settle inside `STARTUP_READY_TIMEOUT`. We still
    /// announce the app is running so the user knows it launched, but
    /// avoid claiming readiness.
    Pending,
}

const fn startup_notice_body(notice: StartupNotice) -> &'static str {
    match notice {
        StartupNotice::Ready => "Nagori is running. Clipboard history is ready.",
        StartupNotice::ReadyButDegraded => {
            "Nagori is running, but clipboard capture is currently degraded. Run `nagori doctor` for details."
        }
        StartupNotice::Failed => {
            "Nagori started, but clipboard capture failed to initialise. Run `nagori doctor` for details."
        }
        StartupNotice::Pending => {
            "Nagori started. Clipboard capture is still initialising — run `nagori doctor` if it does not settle."
        }
    }
}

/// Spawn a background task that waits for the capture loop's
/// initialisation to settle, then fires a single OS notification with a
/// body matching the actual outcome.
///
/// Runs on every OS: macOS routes through `UNUserNotificationCenter`,
/// Windows through the Toast Notifications COM API, and Linux through
/// `org.freedesktop.Notifications` (libnotify). The notification plugin
/// no-ops if the user has not granted permission yet (or, on Linux, if
/// no notification daemon is running), so this stays best-effort and
/// never blocks startup.
pub(crate) fn spawn_startup_ready_notification(handle: &tauri::AppHandle) {
    use tauri_plugin_notification::NotificationExt;

    let Some(state) = handle.try_state::<AppState>() else {
        // No state means setup() bailed before `manage(state)` ran.
        // There is nothing to gate on; the caller has already aborted.
        return;
    };
    let startup_health = state.runtime.startup_health();
    let capture_health = state.runtime.capture_health();
    let app = handle.clone();
    tauri::async_runtime::spawn(async move {
        let notice = await_startup_outcome(
            startup_health,
            capture_health,
            STARTUP_READY_TIMEOUT,
            STARTUP_READY_POLL_INTERVAL,
            CAPTURE_READY_SETTLE_WINDOW,
        )
        .await;
        let _ = app
            .notification()
            .builder()
            .title("Nagori")
            .body(startup_notice_body(notice))
            .show();
    });
}

/// Poll the startup and capture health handles until startup reports an
/// outcome or the timeout fires. Extracted so the polling cadence, the
/// timeout-vs-failure branching, and the "ready but already degraded"
/// case are unit-testable without a Tauri runtime or OS notification
/// daemon.
///
/// `capture_health` is consulted *after* startup reports `ready`: once
/// startup flips, we keep polling capture for up to
/// `capture_settle_window` until the loop either lands a successful
/// tick (`last_success_at` set) or accumulates enough failures to flip
/// `degraded`. Without that window the body would always claim
/// readiness even when the first ticks are silently erroring —
/// `StartupHealth` goes ready the moment settings load returns, which
/// is before any polling tick has run, so `degraded` is structurally
/// false at that point regardless of what's about to happen. The
/// settle window lets the true degraded state surface; if no tick
/// fires inside the window we fall back to `Ready` and let
/// `nagori doctor` reflect the eventual outcome.
async fn await_startup_outcome(
    startup_health: nagori_daemon::StartupHealth,
    capture_health: nagori_daemon::CaptureHealth,
    timeout: std::time::Duration,
    poll_interval: std::time::Duration,
    capture_settle_window: std::time::Duration,
) -> StartupNotice {
    let started = std::time::Instant::now();
    let mut ready_observed_at: Option<std::time::Instant> = None;
    loop {
        let report = startup_health.report();
        if report.last_error.is_some() {
            return StartupNotice::Failed;
        }
        if report.ready {
            let capture = capture_health.report();
            if capture.degraded {
                return StartupNotice::ReadyButDegraded;
            }
            if capture.last_success_at.is_some() {
                return StartupNotice::Ready;
            }
            let waited = ready_observed_at
                .get_or_insert_with(std::time::Instant::now)
                .elapsed();
            if waited >= capture_settle_window {
                return StartupNotice::Ready;
            }
        }
        if started.elapsed() >= timeout {
            return StartupNotice::Pending;
        }
        tokio::time::sleep(poll_interval).await;
    }
}

/// Fire a one-shot background updater probe at launch and surface the
/// result via an OS notification (consistent with how capture / AI
/// transitions are signalled). The notification is best-effort —
/// permission may be denied, and a transient network failure should not
/// pop a scary banner. The download/install hand-off remains
/// user-confirmed via the manual `commands::check_for_updates` trigger.
pub(crate) fn spawn_startup_update_probe(app: &tauri::AppHandle) {
    use tauri_plugin_notification::NotificationExt;
    use tauri_plugin_updater::UpdaterExt;

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let updater = match app.updater() {
            Ok(updater) => updater,
            Err(err) => {
                tracing::warn!(error = %err, "startup_update_probe_unavailable");
                return;
            }
        };
        match updater.check().await {
            Ok(Some(update)) => {
                let _ = app
                    .notification()
                    .builder()
                    .title("Nagori update available")
                    .body(format!(
                        "Version {} is ready. Open Settings → Advanced → Updates to learn more.",
                        update.version
                    ))
                    .show();
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(error = %err, "startup_update_probe_failed");
            }
        }
    });
}

#[cfg(test)]
mod startup_notice_tests {
    use super::{
        CAPTURE_READY_SETTLE_WINDOW, STARTUP_READY_POLL_INTERVAL, STARTUP_READY_TIMEOUT,
        StartupNotice, await_startup_outcome, startup_notice_body,
    };
    use nagori_daemon::{CAPTURE_DEGRADED_THRESHOLD, CaptureHealth, StartupHealth};
    use nagori_ipc::CaptureEventCategory;
    use std::time::Duration;
    use time::OffsetDateTime;

    #[test]
    fn body_distinguishes_each_outcome() {
        // The four bodies have to read as distinct user-facing
        // messages — the whole point of gating the notification is that
        // "ready" can no longer be claimed when capture aborted or is
        // silently degraded. Lock the strings here so a copy-edit to
        // one doesn't accidentally collide with another and re-introduce
        // the original bug.
        let ready = startup_notice_body(StartupNotice::Ready);
        let degraded = startup_notice_body(StartupNotice::ReadyButDegraded);
        let failed = startup_notice_body(StartupNotice::Failed);
        let pending = startup_notice_body(StartupNotice::Pending);
        assert!(ready.contains("ready"));
        assert!(
            !ready.contains("failed"),
            "ready body must not imply failure"
        );
        assert!(degraded.contains("degraded"));
        assert!(degraded.contains("nagori doctor"));
        assert!(failed.contains("failed"));
        assert!(failed.contains("nagori doctor"));
        assert!(pending.contains("initialising"));
        assert!(pending.contains("nagori doctor"));
        // Distinctness: a future edit collapsing two bodies onto the
        // same string would silently reintroduce the "always says
        // ready" UX bug for one of the outcomes.
        for (a, b) in [
            (ready, degraded),
            (ready, failed),
            (ready, pending),
            (degraded, failed),
            (degraded, pending),
            (failed, pending),
        ] {
            assert_ne!(a, b);
        }
    }

    #[tokio::test]
    async fn await_outcome_returns_ready_when_capture_succeeds() {
        let health = StartupHealth::new();
        health.record_capture_ready();
        let capture = CaptureHealth::new();
        // Simulate the loop having landed at least one healthy tick
        // before the gate inspects health — that's the production
        // signal that takes precedence over the settle window.
        capture.record_success(OffsetDateTime::now_utc());
        let notice = await_startup_outcome(
            health,
            capture,
            Duration::from_secs(1),
            Duration::from_millis(5),
            Duration::from_millis(50),
        )
        .await;
        assert_eq!(notice, StartupNotice::Ready);
    }

    #[tokio::test]
    async fn await_outcome_returns_ready_but_degraded_when_capture_already_failing() {
        // Startup announced ready, but the capture loop has already
        // tripped its degraded threshold inside the init window. The
        // notification must not claim "ready" — that was the silent
        // data-loss bug `CaptureHealth` exists to surface.
        let health = StartupHealth::new();
        health.record_capture_ready();
        let capture = CaptureHealth::new();
        for _ in 0..CAPTURE_DEGRADED_THRESHOLD {
            capture.record_error(
                CaptureEventCategory::Adapter,
                "ax read failed",
                OffsetDateTime::now_utc(),
            );
        }
        let notice = await_startup_outcome(
            health,
            capture,
            Duration::from_secs(1),
            Duration::from_millis(5),
            Duration::from_millis(50),
        )
        .await;
        assert_eq!(notice, StartupNotice::ReadyButDegraded);
    }

    #[tokio::test]
    async fn await_outcome_returns_failed_when_capture_aborts() {
        let health = StartupHealth::new();
        health.record_capture_failed("could not load settings");
        let capture = CaptureHealth::new();
        let notice = await_startup_outcome(
            health,
            capture,
            Duration::from_secs(1),
            Duration::from_millis(5),
            Duration::from_millis(50),
        )
        .await;
        assert_eq!(notice, StartupNotice::Failed);
    }

    #[tokio::test]
    async fn await_outcome_waits_for_late_signal() {
        // Mirrors the real flow: the spawn_background_tasks task posts
        // its outcome after the notification probe is already polling.
        // Without the polling loop the gate would short-circuit to
        // `Pending` and the user would see the wrong body.
        let health = StartupHealth::new();
        let signal = health.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            signal.record_capture_ready();
        });
        let capture = CaptureHealth::new();
        capture.record_success(OffsetDateTime::now_utc());
        let notice = await_startup_outcome(
            health,
            capture,
            Duration::from_secs(2),
            Duration::from_millis(10),
            Duration::from_millis(50),
        )
        .await;
        assert_eq!(notice, StartupNotice::Ready);
    }

    #[tokio::test]
    async fn await_outcome_falls_back_to_pending_after_timeout() {
        // Wedged init: no outcome posted before the cap. The body must
        // not claim readiness, but also must not pretend a failure was
        // recorded — `Pending` is the honest answer.
        let health = StartupHealth::new();
        let capture = CaptureHealth::new();
        let notice = await_startup_outcome(
            health,
            capture,
            Duration::from_millis(40),
            Duration::from_millis(10),
            Duration::from_millis(20),
        )
        .await;
        assert_eq!(notice, StartupNotice::Pending);
    }

    #[tokio::test]
    async fn await_outcome_waits_for_first_capture_outcome_after_ready() {
        // Regression: when startup flips ready before any polling tick
        // has run, `capture_health.degraded` is structurally false
        // (counter is 0) even if the first three ticks are about to
        // all error. The gate must keep watching capture until either
        // a tick succeeds or the threshold flips — short-circuiting on
        // ready alone resurrects the silent-data-loss bug.
        let startup = StartupHealth::new();
        startup.record_capture_ready();
        let capture = CaptureHealth::new();
        let signal = capture.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(40)).await;
            for _ in 0..CAPTURE_DEGRADED_THRESHOLD {
                signal.record_error(
                    CaptureEventCategory::Adapter,
                    "late adapter error",
                    OffsetDateTime::now_utc(),
                );
            }
        });
        let notice = await_startup_outcome(
            startup,
            capture,
            Duration::from_secs(1),
            Duration::from_millis(5),
            Duration::from_millis(500),
        )
        .await;
        assert_eq!(notice, StartupNotice::ReadyButDegraded);
    }

    #[tokio::test]
    async fn await_outcome_falls_back_to_ready_when_settle_window_elapses_idle() {
        // The capture loop may be running at a slow cadence (or the
        // host is genuinely idle) and produce no outcomes inside the
        // settle window. Treating that as "ready" is the only honest
        // answer — `nagori doctor` continues to report the eventual
        // state once a tick lands.
        let startup = StartupHealth::new();
        startup.record_capture_ready();
        let capture = CaptureHealth::new();
        let notice = await_startup_outcome(
            startup,
            capture,
            Duration::from_secs(1),
            Duration::from_millis(5),
            Duration::from_millis(40),
        )
        .await;
        assert_eq!(notice, StartupNotice::Ready);
    }

    #[test]
    fn timeout_constants_are_reasonable() {
        // Guard against accidental edits that would make the cap so
        // short that a cold SQLite open always falls back to `Pending`
        // (defeating the gate), or so long that a real failure never
        // surfaces a notification within a useful window. The settle
        // window has to fit inside the cap and be long enough to cover
        // a few default-cadence ticks (500 ms × 3 = 1.5 s degraded
        // latency).
        assert!(STARTUP_READY_TIMEOUT >= Duration::from_secs(2));
        assert!(STARTUP_READY_TIMEOUT <= Duration::from_secs(30));
        assert!(STARTUP_READY_POLL_INTERVAL <= Duration::from_millis(500));
        assert!(CAPTURE_READY_SETTLE_WINDOW >= Duration::from_millis(1_500));
        assert!(CAPTURE_READY_SETTLE_WINDOW < STARTUP_READY_TIMEOUT);
    }
}
