use std::sync::Mutex;
use std::time::Duration;

use arboard::Clipboard;
use async_trait::async_trait;
use nagori_core::{
    AppError, ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
    ReadBudget, Result,
};
use nagori_platform::{
    CapturedSnapshot, ClipboardReader, SNAPSHOT_CAPTURE_MAX_RETRIES, clipboard_blocking,
    lock_clipboard_recovering, platform_err,
};
use time::OffsetDateTime;

#[cfg(target_os = "macos")]
use super::MAX_TEXT_REP_BYTES;
#[cfg(target_os = "macos")]
use super::file_url::{oversized_payload, pasteboard_exclusion, plain_text_byte_len};
#[cfg(target_os = "macos")]
use super::transcode::collect_macos_extras;
use super::transcode::{finalize_captured, transcode_snapshot};
use super::{MacosClipboard, pasteboard_sequence};

/// Brief pause between torn-snapshot retries.
///
/// Three immediate retries during a foreign write storm burn out in a
/// sub-millisecond window and all observe the same torn state. A short sleep
/// (the read runs on the blocking pool, so sleeping is fine) lets the foreign
/// writer settle, raising the odds the next attempt reads a stable changeCount.
const TORN_RETRY_BACKOFF: Duration = Duration::from_millis(1);

#[async_trait]
impl ClipboardReader for MacosClipboard {
    async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
        // arboard + AppKit pasteboard reads are synchronous and can take
        // several milliseconds when the source app is slow to publish. Run
        // them on the blocking pool so a stuck pasteboard never pins a
        // tokio worker thread (the daemon only has a handful of workers,
        // and a stuck `current_snapshot` previously starved IPC handlers).
        //
        // Funnel through the same `capture_attempt` machinery as the bounded
        // path (passing `None` for "no size budget") so the unbounded read no
        // longer drifts from it: it now gets torn-snapshot retry and the
        // owner-exclusion check for free, instead of sampling the sequence once
        // at the end and trusting it.
        let clipboard = self.clipboard.clone();
        let captured = clipboard_blocking("current_snapshot", move || {
            capture_snapshot_attempts(&clipboard)
        })
        .await
        .map_err(|err| AppError::Platform(err.to_string()))??;
        // Map the captured result back to a plain snapshot. The unbounded path
        // has no budget, so `Oversized` cannot occur; an `Excluded` clip yields
        // an empty snapshot — we never materialise the secret body.
        let snapshot = match captured {
            CapturedSnapshot::Captured(snapshot) => snapshot,
            CapturedSnapshot::Excluded { sequence, .. } => ClipboardSnapshot {
                sequence,
                captured_at: OffsetDateTime::now_utc(),
                source: None,
                representations: Vec::new(),
            },
            CapturedSnapshot::Oversized { .. } => {
                unreachable!("the unbounded current_snapshot path has no size budget")
            }
        };
        // Normalise any captured TIFF to PNG off the read timeout — the raw
        // bytes are already captured (and torn-checked) under the lock above.
        transcode_snapshot(snapshot).await
    }

    async fn current_sequence(&self) -> Result<ClipboardSequence> {
        // `NSPasteboard::changeCount` is cheap, but it still touches AppKit
        // global state. Hop to a blocking thread for consistency with
        // `current_snapshot` so the polling loop can never block a tokio
        // worker even if AppKit hits an internal lock.
        clipboard_blocking("current_sequence", pasteboard_sequence)
            .await
            .map_err(|err| AppError::Platform(err.to_string()))
    }

    async fn current_snapshot_with_max(&self, budget: ReadBudget) -> Result<CapturedSnapshot> {
        let clipboard = self.clipboard.clone();
        let captured = clipboard_blocking("current_snapshot_with_max", move || {
            capture_snapshot_with_max(&clipboard, budget)
        })
        .await
        .map_err(|err| AppError::Platform(err.to_string()))??;
        // Normalise any captured TIFF to PNG off the read timeout, then
        // re-apply the image budget to the transcoded image (see
        // `finalize_captured`).
        finalize_captured(captured, budget).await
    }
}

/// What one bounded capture attempt observed.
///
/// `Torn` means the pasteboard `changeCount` drifted between the attempt's
/// `before` baseline and its final sample — the collected representations
/// (or the oversize observation) may be stitched across two distinct
/// publish events. The attempt's result is still carried so the *final*
/// retry can accept it (matching Windows' behaviour): torn snapshots
/// surface as a normal entry rather than as a hard error that pauses
/// capture. Returning the torn `Oversized` sequence matters for the same
/// reason as on the clean path — anchoring `last_sequence` to an older
/// changeCount would make the capture loop skip the next clip, because it
/// dedupes on sequence equality.
pub(super) enum CaptureAttempt {
    Settled(CapturedSnapshot),
    Torn(CapturedSnapshot),
}

/// Bounded snapshot read with torn-snapshot retry.
///
/// Same locking discipline as `current_snapshot` — each attempt holds the
/// arboard mutex across both the `AppKit` size probe and the per-rep load so
/// a concurrent writer cannot race a torn snapshot in between. The arboard
/// mutex protects us against same-process writes, but any other macOS app
/// can still publish onto the shared `NSPasteboard` mid-load; mirror the
/// Windows `before == after` check (see
/// `crates/nagori-platform-windows/src/clipboard.rs::capture_snapshot`) to
/// catch torn snapshots and retry rather than store a stitched entry whose
/// representations came from different writes. Bounded to `MAX_RETRIES` so
/// a write storm can't park the capture loop here forever; the final
/// attempt accepts whatever it observed.
fn capture_snapshot_with_max(
    clipboard: &Mutex<Clipboard>,
    budget: ReadBudget,
) -> Result<CapturedSnapshot> {
    resolve_capture_attempts(SNAPSHOT_CAPTURE_MAX_RETRIES, TORN_RETRY_BACKOFF, || {
        capture_attempt(clipboard, Some(budget))
    })
}

/// Unbounded snapshot read with the same torn-snapshot retry as
/// [`capture_snapshot_with_max`], but without a size budget (`max_bytes =
/// None`). Backs `current_snapshot`; the per-rep defence-in-depth ceilings
/// ([`MAX_TEXT_REP_BYTES`] / `MAX_IMAGE_REP_BYTES`) still bound memory.
fn capture_snapshot_attempts(clipboard: &Mutex<Clipboard>) -> Result<CapturedSnapshot> {
    resolve_capture_attempts(SNAPSHOT_CAPTURE_MAX_RETRIES, TORN_RETRY_BACKOFF, || {
        capture_attempt(clipboard, None)
    })
}

/// Drive the torn-snapshot retry loop: return the first `Settled` snapshot, or
/// — once `max_retries` is exhausted — the last `Torn` one (anchoring
/// `last_sequence` to the freshest changeCount we saw). Decoupled from the
/// pasteboard so the orchestration is exercised with a scripted attempt source
/// instead of a live `NSPasteboard` racing a foreign writer.
pub(super) fn resolve_capture_attempts(
    max_retries: usize,
    backoff: Duration,
    mut attempt: impl FnMut() -> Result<CaptureAttempt>,
) -> Result<CapturedSnapshot> {
    for n in 1..=max_retries {
        match attempt()? {
            CaptureAttempt::Settled(snapshot) => return Ok(snapshot),
            CaptureAttempt::Torn(snapshot) => {
                if n == max_retries {
                    return Ok(snapshot);
                }
                // Foreign writer landed mid-attempt — back off briefly so a
                // write storm doesn't consume every retry in the same instant,
                // then discard and retry. Tests pass `Duration::ZERO`.
                if !backoff.is_zero() {
                    std::thread::sleep(backoff);
                }
            }
        }
    }
    unreachable!("the final retry returns its result unconditionally")
}

/// Read the plain-text (`text/plain`) representation under the arboard lock,
/// applying the unbounded-path defence-in-depth text ceiling.
///
/// On the `None` (budget-less `current_snapshot`) path a probe of the
/// pasteboard's plain-text byte length gates the read: a payload over
/// [`MAX_TEXT_REP_BYTES`] is dropped *before* `get_text` copies it, so a
/// hostile multi-GB string never lands in the daemon's heap. The bounded path
/// is already covered by `oversized_payload`, so it skips the probe.
#[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
fn read_plain_text(guard: &mut Clipboard, budget: Option<ReadBudget>) -> Result<Option<String>> {
    #[cfg(target_os = "macos")]
    if budget.is_none() && plain_text_byte_len().is_some_and(|len| len > MAX_TEXT_REP_BYTES) {
        tracing::warn!(
            ceiling = MAX_TEXT_REP_BYTES,
            "pasteboard_text_rep_exceeds_ceiling"
        );
        return Ok(None);
    }
    match guard.get_text() {
        Ok(text) => Ok(Some(text)),
        Err(arboard::Error::ContentNotAvailable) => Ok(None),
        Err(err) => Err(platform_err(&err)),
    }
}

/// One probe → load → verify pass over the pasteboard.
///
/// `budget` is `Some` on the bounded `current_snapshot_with_max` path (an
/// over-budget payload short-circuits to `Oversized`, sizing image bytes
/// against `budget.image_bytes` and everything else against
/// `budget.text_bytes`) and `None` on the unbounded `current_snapshot` path
/// (no budget to reject against — oversized reps are instead dropped by the
/// per-rep defence-in-depth ceilings).
#[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
fn capture_attempt(
    clipboard: &Mutex<Clipboard>,
    budget: Option<ReadBudget>,
) -> Result<CaptureAttempt> {
    // Recover from a poisoned guard rather than failing the read: the arboard
    // clipboard has no Rust invariant a prior panic could leave broken, and
    // erroring here would wedge every later capture behind one historical panic.
    let mut guard = lock_clipboard_recovering(clipboard);
    let before = pasteboard_sequence();

    // Owner-declared exclusion marker (nspasteboard.org Concealed / Transient)
    // takes precedence over everything else: a password manager's secret is
    // skipped *before* `get_text` reads it, so in the common case the secret
    // never enters our address space (a marker that only becomes visible after
    // this point is caught by the post-read re-check below). Treated as a
    // settled-or-torn outcome like `Oversized` so a foreign write mid-attempt
    // retries rather than acting on a stale type list.
    #[cfg(target_os = "macos")]
    if let Some(kind) = pasteboard_exclusion() {
        let after = pasteboard_sequence();
        drop(guard);
        return Ok(settle(
            &before,
            &after,
            CapturedSnapshot::Excluded {
                sequence: after.clone(),
                kind,
            },
        ));
    }

    // First pass: peek byte sizes without materialising payloads. On
    // macOS, NSData backs each `dataForType` result with bytes
    // already paged into our address space, but skipping `to_vec()`
    // still avoids the second copy into a Rust `Vec<u8>` and lets
    // NSData drop on scope exit, freeing both copies promptly.
    // NSString::len() reports UTF-8 bytes without materialising a
    // Rust String. This pass is still only an admission pre-filter:
    // it catches oversized single reps and file URL aggregates
    // before we allocate Rust payload buffers, while the capture
    // loop's post-load check remains authoritative for the final
    // ClipboardEntry payload.
    #[cfg(target_os = "macos")]
    if let Some(budget) = budget
        && let Some((observed, limit)) = oversized_payload(budget)
    {
        let after = pasteboard_sequence();
        drop(guard);
        return Ok(settle(
            &before,
            &after,
            CapturedSnapshot::Oversized {
                sequence: after.clone(),
                observed_bytes: observed,
                limit,
            },
        ));
    }

    // Second pass: load the snapshot. The first pass only rejected
    // the obvious oversize cases; reps that pass it can still grow
    // past `max_bytes` once decoded to UTF-8, and the aggregate
    // of multiple reps is not bounded here at all. The capture
    // loop's post-load `payload_bytes > max_entry_size_bytes`
    // check is the authoritative limit — the first pass just spares
    // us the worst allocations.
    //
    // On the unbounded path there is no `oversized_payload` pre-filter, so
    // `read_plain_text` applies a defence-in-depth text ceiling before
    // `get_text` copies the payload (the bounded path is already covered by the
    // pre-filter above).
    let plain = read_plain_text(&mut guard, budget)?;

    let mut representations = Vec::new();

    // File URLs are text-kind, so they answer to the text budget; image reps
    // collected here are bounded by the defence-in-depth `MAX_IMAGE_REP_BYTES`
    // ceiling and re-checked against the image budget in `finalize_captured`.
    #[cfg(target_os = "macos")]
    let collected_oversize =
        collect_macos_extras(&mut representations, budget.map(|b| b.text_bytes));
    #[cfg(target_os = "macos")]
    if let (Some(budget), Some(observed)) = (budget, collected_oversize) {
        let after = pasteboard_sequence();
        drop(guard);
        return Ok(settle(
            &before,
            &after,
            CapturedSnapshot::Oversized {
                sequence: after.clone(),
                observed_bytes: observed,
                limit: budget.text_bytes,
            },
        ));
    }

    if let Some(text) = plain {
        representations.push(ClipboardRepresentation {
            mime_type: "text/plain".to_owned(),
            data: ClipboardData::Text(text),
        });
    }

    // Re-check the exclusion marker *after* the body read. The pre-read probe
    // and `get_text` are two separate pasteboard queries, so a marker can
    // appear between them within a single publish (macOS folds a
    // clear-then-write into one `changeCount`, and the final torn retry below
    // accepts whatever it read). Re-probing here binds the skip decision to a
    // post-read confirmation: the `representations` we just built — including
    // any secret body — are dropped unreturned, so a marked clip is never
    // emitted to the capture loop even when it landed mid-attempt.
    //
    // This covers every single-publish ordering (the concealed *type* is
    // observable at-or-before its data under both `writeObjects` and
    // `declareTypes` + `setData`, so a body we could read was always
    // accompanied by a marker one of the two probes sees). The one residual is
    // a *multi*-publish torn race — an unmarked clip, then a marked one whose
    // body `get_text` samples, then another unmarked one before this probe —
    // on the final retry, where `before != after` is accepted unconditionally
    // below. That requires three foreign publishes inside this sub-millisecond
    // attempt and is the same torn-snapshot tradeoff every capture makes; we
    // accept it rather than dropping every torn body.
    #[cfg(target_os = "macos")]
    if let Some(kind) = pasteboard_exclusion() {
        let after = pasteboard_sequence();
        drop(guard);
        return Ok(settle(
            &before,
            &after,
            CapturedSnapshot::Excluded {
                sequence: after.clone(),
                kind,
            },
        ));
    }

    let after = pasteboard_sequence();
    drop(guard);
    Ok(settle(
        &before,
        &after,
        CapturedSnapshot::Captured(ClipboardSnapshot {
            sequence: after.clone(),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations,
        }),
    ))
}

/// Classify an attempt's result by whether the changeCount stayed stable
/// across it.
pub(super) fn settle(
    before: &ClipboardSequence,
    after: &ClipboardSequence,
    snapshot: CapturedSnapshot,
) -> CaptureAttempt {
    if before == after {
        CaptureAttempt::Settled(snapshot)
    } else {
        CaptureAttempt::Torn(snapshot)
    }
}
