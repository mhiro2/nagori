use super::super::read::{CaptureAttempt, resolve_capture_attempts, settle};
use super::super::*;

use nagori_platform::CapturedSnapshot;

/// A `CapturedSnapshot` tagged by sequence so tests can assert *which*
/// attempt's snapshot the retry loop returned.
fn tagged(tag: &str) -> CapturedSnapshot {
    CapturedSnapshot::Oversized {
        sequence: ClipboardSequence::content_hash(tag),
        observed_bytes: 1,
        limit: 1,
    }
}

fn snapshot_tag(snapshot: &CapturedSnapshot) -> Option<ClipboardSequence> {
    match snapshot {
        CapturedSnapshot::Oversized { sequence, .. } => Some(sequence.clone()),
        _ => None,
    }
}

#[test]
fn settle_classifies_by_changecount_stability() {
    let a = ClipboardSequence::content_hash("a");
    let b = ClipboardSequence::content_hash("b");
    assert!(matches!(
        settle(&a, &a, tagged("x")),
        CaptureAttempt::Settled(_)
    ));
    assert!(matches!(
        settle(&a, &b, tagged("x")),
        CaptureAttempt::Torn(_)
    ));
}

#[test]
fn resolve_capture_attempts_returns_the_first_settled() {
    let mut calls = 0;
    let result = resolve_capture_attempts(3, std::time::Duration::ZERO, || {
        calls += 1;
        Ok(CaptureAttempt::Settled(tagged("settled")))
    })
    .expect("settled attempt");
    assert_eq!(calls, 1, "a settled first attempt stops immediately");
    assert_eq!(
        snapshot_tag(&result),
        Some(ClipboardSequence::content_hash("settled"))
    );
}

#[test]
fn resolve_capture_attempts_retries_past_torn_until_settled() {
    let mut calls = 0;
    let result = resolve_capture_attempts(3, std::time::Duration::ZERO, || {
        calls += 1;
        if calls < 3 {
            Ok(CaptureAttempt::Torn(tagged("torn")))
        } else {
            Ok(CaptureAttempt::Settled(tagged("settled")))
        }
    })
    .expect("eventually settles");
    assert_eq!(calls, 3, "retries through the torn attempts");
    assert_eq!(
        snapshot_tag(&result),
        Some(ClipboardSequence::content_hash("settled"))
    );
}

#[test]
fn resolve_capture_attempts_accepts_the_last_torn_when_retries_exhaust() {
    let mut calls = 0;
    let result = resolve_capture_attempts(3, std::time::Duration::ZERO, || {
        calls += 1;
        Ok(CaptureAttempt::Torn(tagged(&format!("torn{calls}"))))
    })
    .expect("final torn attempt is accepted");
    assert_eq!(calls, 3, "every retry is consumed");
    // The freshest (last) torn snapshot wins so `last_sequence` anchors to
    // the newest changeCount and the next clip is not skipped.
    assert_eq!(
        snapshot_tag(&result),
        Some(ClipboardSequence::content_hash("torn3"))
    );
}

#[test]
fn resolve_capture_attempts_propagates_an_attempt_error() {
    let mut calls = 0;
    let result = resolve_capture_attempts(3, std::time::Duration::ZERO, || {
        calls += 1;
        Err(nagori_core::AppError::Platform("probe failed".to_owned()))
    });
    assert!(result.is_err(), "an attempt error aborts the loop");
    assert_eq!(calls, 1, "the error stops further retries");
}
