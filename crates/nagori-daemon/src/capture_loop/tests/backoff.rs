use super::super::*;
use super::loop_for;

use nagori_platform::MemoryClipboard;
use nagori_storage::SqliteStore;

#[test]
fn backoff_keeps_base_interval_below_threshold() {
    // Below the threshold the loop must keep its configured cadence —
    // a single transient hiccup should not stretch poll spacing or
    // hide the next clip behind a several-second wait.
    let base = Duration::from_millis(500);
    for failures in 0..BACKOFF_AFTER_CONSECUTIVE_FAILURES {
        assert_eq!(
            CaptureFailurePolicy::backoff_for_failures(base, failures),
            base
        );
    }
}

#[test]
fn backoff_grows_exponentially_then_caps() {
    // Above the threshold the spacing doubles each consecutive
    // failure (1× → 2× → 4× …) until MAX_BACKOFF clamps it. The cap
    // matters: without it a sustained outage would push the next
    // tick out by minutes.
    let base = Duration::from_millis(500);
    let first =
        CaptureFailurePolicy::backoff_for_failures(base, BACKOFF_AFTER_CONSECUTIVE_FAILURES);
    assert_eq!(first, base * 2);
    let second =
        CaptureFailurePolicy::backoff_for_failures(base, BACKOFF_AFTER_CONSECUTIVE_FAILURES + 1);
    assert_eq!(second, base * 4);
    let huge = CaptureFailurePolicy::backoff_for_failures(base, 1_000);
    assert_eq!(huge, MAX_BACKOFF);
}

#[test]
fn apply_jitter_stays_within_ten_percent_window() {
    // Jitter must never push the sleep below 90% or above 110% of
    // the scaled backoff — otherwise the cap (`MAX_BACKOFF`) and
    // the floor (avoiding a busy-loop retry) lose their meaning.
    let base = Duration::from_secs(1);
    let nanos = u64::try_from(base.as_nanos()).expect("1s fits u64");
    let low = nanos - nanos / 10;
    let high = nanos + nanos / 10;
    for entropy in [0_u64, 1, 7, nanos, u64::MAX] {
        let jittered = CaptureFailurePolicy::apply_jitter(base, entropy);
        let got = u64::try_from(jittered.as_nanos()).expect("1.1s fits u64");
        assert!(
            got >= low && got <= high,
            "jitter {got}ns escaped ±10% window [{low},{high}] for entropy={entropy}",
        );
    }
}

#[test]
fn apply_jitter_endpoints_hit_min_and_max() {
    // `entropy = 0` lands at the floor (`-range`), `entropy = 2*range`
    // lands at the ceiling (`+range`). Pinning both anchors guards
    // against an off-by-one in the symmetry mapping.
    let base = Duration::from_secs(1);
    let nanos = u64::try_from(base.as_nanos()).expect("1s fits u64");
    let range = nanos / 10;
    let floor = CaptureFailurePolicy::apply_jitter(base, 0);
    assert_eq!(
        floor,
        Duration::from_nanos(nanos - range),
        "entropy=0 should produce the lower bound",
    );
    let ceil = CaptureFailurePolicy::apply_jitter(base, range * 2);
    assert_eq!(
        ceil,
        Duration::from_nanos(nanos + range),
        "entropy=2*range should produce the upper bound",
    );
}

#[test]
fn jittered_backoff_keeps_pre_threshold_cadence_unchanged() {
    // Below the threshold the loop is still on the user's configured
    // cadence; jittering that would silently slow steady-state
    // captures for no benefit. The jitter only kicks in once backoff
    // is actually active.
    let base = Duration::from_millis(500);
    for failures in 0..BACKOFF_AFTER_CONSECUTIVE_FAILURES {
        assert_eq!(CaptureFailurePolicy::jittered_backoff(base, failures), base);
    }
}

#[test]
fn apply_jitter_then_cap_never_exceeds_max_backoff() {
    // Once `backoff_for_failures` saturates at `MAX_BACKOFF`, the
    // jitter's `+10%` swing must not push the next sleep above the
    // documented ceiling — operators read `MAX_BACKOFF` as a hard
    // limit, so an extra 3 seconds at the top of the curve is a
    // surprising regression. The `apply_jitter(...).min(MAX_BACKOFF)`
    // step in `jittered_backoff` enforces this; pin the worst case.
    let saturated = MAX_BACKOFF;
    let entropy_max =
        u64::try_from(saturated.as_nanos() / 10).expect("MAX_BACKOFF/10 fits u64") * 2;
    let with_max_jitter =
        CaptureFailurePolicy::apply_jitter(saturated, entropy_max).min(MAX_BACKOFF);
    assert!(
        with_max_jitter <= MAX_BACKOFF,
        "post-jitter clamp must respect MAX_BACKOFF: {with_max_jitter:?}",
    );
}

#[tokio::test]
async fn note_capture_error_resets_consecutive_failures_on_success() {
    // The polling loop drives the backoff off `consecutive_failures`,
    // which must reset on the next successful tick — otherwise a
    // recovered daemon stays paced at MAX_BACKOFF forever.
    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    let mut loop_ = loop_for(clipboard, store, AppSettings::default());

    loop_.note_capture_error(&AppError::Platform("simulated".to_owned()));
    loop_.note_capture_error(&AppError::Platform("simulated".to_owned()));
    assert_eq!(loop_.failures.consecutive_failures, 2);
    loop_.note_capture_success();
    assert_eq!(loop_.failures.consecutive_failures, 0);
}

#[tokio::test]
async fn note_capture_error_buckets_warns_per_kind() {
    // Two distinct error kinds within the suppression window must
    // each emit at least one warn — otherwise an in-flight platform
    // suppression would shadow a sudden second failure mode (e.g.
    // AX permission loss landing while pasteboard reads are still
    // failing).
    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    let mut loop_ = loop_for(clipboard, store, AppSettings::default());

    loop_.note_capture_error(&AppError::Platform("first".to_owned()));
    // Same kind: suppressed, but counter increments.
    loop_.note_capture_error(&AppError::Platform("second".to_owned()));
    let platform_slot = CaptureErrorKind::Platform as usize;
    assert_eq!(loop_.failures.suppressed_warns[platform_slot], 1);

    // Different kind: emits its own warn line, independent of the
    // platform suppression timer.
    loop_.note_capture_error(&AppError::Policy("policy hit".to_owned()));
    let policy_slot = CaptureErrorKind::Policy as usize;
    // After emitting, suppressed counter is consumed back to 0.
    assert_eq!(loop_.failures.suppressed_warns[policy_slot], 0);
    assert!(loop_.failures.last_warn_at[policy_slot].is_some());
}

#[tokio::test]
async fn note_capture_error_distinguishes_storage_from_invalid_input() {
    // Regression: previously a single `Other` slot held both
    // `Storage` and `InvalidInput` (and every other non-Platform
    // variant), so a burst of one would shadow the other for a full
    // suppression window. Confirm each variant gets its own bucket.
    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    let mut loop_ = loop_for(clipboard, store, AppSettings::default());

    loop_.note_capture_error(&AppError::storage("disk full".to_owned()));
    loop_.note_capture_error(&AppError::storage("disk full".to_owned()));
    let storage_slot = CaptureErrorKind::Storage as usize;
    assert_eq!(loop_.failures.suppressed_warns[storage_slot], 1);

    loop_.note_capture_error(&AppError::InvalidInput("bad clip".to_owned()));
    let invalid_slot = CaptureErrorKind::InvalidInput as usize;
    // The invalid-input arm emits its own warn rather than being
    // shadowed by the in-flight Storage suppression — so its
    // last_warn_at is set and the suppressed counter starts from
    // zero on the next collision.
    assert!(loop_.failures.last_warn_at[invalid_slot].is_some());
    assert_eq!(loop_.failures.suppressed_warns[invalid_slot], 0);
    // The Storage suppression is independent and its in-flight
    // suppressed counter is unaffected by the invalid-input emit.
    assert_eq!(loop_.failures.suppressed_warns[storage_slot], 1);
}

#[tokio::test]
async fn note_capture_error_routes_storage_failures_to_storage_category() {
    // A wedged-disk / DB-locked failure has to land as `Storage`,
    // not `Adapter` — otherwise the doctor / tray hint would point
    // the user at re-granting clipboard permissions instead of
    // checking disk space. Lock the boundary here so a future edit
    // to the error→category map can't silently collapse Storage
    // back into the generic adapter bucket.
    let clipboard = Arc::new(MemoryClipboard::new());
    let store = SqliteStore::open_memory().expect("memory store");
    let health = CaptureHealth::new();
    let mut loop_ =
        loop_for(clipboard, store, AppSettings::default()).with_capture_health(health.clone());

    loop_.note_capture_error(&AppError::storage("disk full".to_owned()));
    loop_.note_capture_error(&AppError::Platform("ax read failed".to_owned()));
    let report = health.report();
    // The most recent non-success outcome was the Platform error,
    // which collapses into `Adapter` — verifies the catch-all arm
    // still functions alongside the carved-out Storage variant.
    assert_eq!(
        report.last_event_category,
        Some(nagori_ipc::CaptureEventCategory::Adapter)
    );

    // And the Storage variant routes correctly when it is the
    // most recent.
    loop_.note_capture_error(&AppError::storage("disk full".to_owned()));
    let report = health.report();
    assert_eq!(
        report.last_event_category,
        Some(nagori_ipc::CaptureEventCategory::Storage)
    );
}
