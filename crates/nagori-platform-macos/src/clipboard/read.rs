use std::sync::Mutex;

use arboard::Clipboard;
use async_trait::async_trait;
use nagori_core::{
    AppError, ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot, Result,
};
use nagori_platform::{
    CapturedSnapshot, ClipboardReader, SNAPSHOT_CAPTURE_MAX_RETRIES, clipboard_blocking, lock_err,
    platform_err,
};
use time::OffsetDateTime;

#[cfg(target_os = "macos")]
use super::file_url::{oversized_payload, pasteboard_exclusion};
#[cfg(target_os = "macos")]
use super::transcode::collect_macos_extras;
use super::transcode::{finalize_captured, transcode_snapshot};
use super::{MacosClipboard, pasteboard_sequence};

#[async_trait]
impl ClipboardReader for MacosClipboard {
    async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
        // arboard + AppKit pasteboard reads are synchronous and can take
        // several milliseconds when the source app is slow to publish. Run
        // them on the blocking pool so a stuck pasteboard never pins a
        // tokio worker thread (the daemon only has a handful of workers,
        // and a stuck `current_snapshot` previously starved IPC handlers).
        let clipboard = self.clipboard.clone();
        let snapshot =
            clipboard_blocking("current_snapshot", move || -> Result<ClipboardSnapshot> {
                // Hold the arboard mutex across `get_text` *and* the AppKit
                // extras read so a concurrent `write_image_bytes` cannot slip
                // its `clearContents`/`setData` pair between the two and stitch
                // a torn snapshot (e.g. old text paired with new image, or an
                // empty pasteboard observed mid-write).
                let mut guard = clipboard.lock().map_err(|err| lock_err(&err))?;
                let plain = match guard.get_text() {
                    Ok(text) => Some(text),
                    Err(arboard::Error::ContentNotAvailable) => None,
                    Err(err) => return Err(platform_err(&err)),
                };

                let mut representations = Vec::new();

                #[cfg(target_os = "macos")]
                let _ = collect_macos_extras(&mut representations, None);

                if let Some(text) = plain {
                    representations.push(ClipboardRepresentation {
                        mime_type: "text/plain".to_owned(),
                        data: ClipboardData::Text(text),
                    });
                }

                let snapshot = ClipboardSnapshot {
                    sequence: pasteboard_sequence(),
                    captured_at: OffsetDateTime::now_utc(),
                    source: None,
                    representations,
                };
                drop(guard);
                Ok(snapshot)
            })
            .await
            .map_err(|err| AppError::Platform(err.to_string()))??;
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

    async fn current_snapshot_with_max(&self, max_bytes: usize) -> Result<CapturedSnapshot> {
        let clipboard = self.clipboard.clone();
        let captured = clipboard_blocking("current_snapshot_with_max", move || {
            capture_snapshot_with_max(&clipboard, max_bytes)
        })
        .await
        .map_err(|err| AppError::Platform(err.to_string()))??;
        // Normalise any captured TIFF to PNG off the read timeout, then
        // re-apply the size budget to the transcoded image (see
        // `finalize_captured`).
        finalize_captured(captured, max_bytes).await
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
    max_bytes: usize,
) -> Result<CapturedSnapshot> {
    resolve_capture_attempts(SNAPSHOT_CAPTURE_MAX_RETRIES, || {
        capture_attempt(clipboard, max_bytes)
    })
}

/// Drive the torn-snapshot retry loop: return the first `Settled` snapshot, or
/// — once `max_retries` is exhausted — the last `Torn` one (anchoring
/// `last_sequence` to the freshest changeCount we saw). Decoupled from the
/// pasteboard so the orchestration is exercised with a scripted attempt source
/// instead of a live `NSPasteboard` racing a foreign writer.
pub(super) fn resolve_capture_attempts(
    max_retries: usize,
    mut attempt: impl FnMut() -> Result<CaptureAttempt>,
) -> Result<CapturedSnapshot> {
    for n in 1..=max_retries {
        match attempt()? {
            CaptureAttempt::Settled(snapshot) => return Ok(snapshot),
            CaptureAttempt::Torn(snapshot) => {
                if n == max_retries {
                    return Ok(snapshot);
                }
                // Foreign writer landed mid-attempt — discard and retry.
            }
        }
    }
    unreachable!("the final retry returns its result unconditionally")
}

/// One probe → load → verify pass over the pasteboard.
#[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
fn capture_attempt(clipboard: &Mutex<Clipboard>, max_bytes: usize) -> Result<CaptureAttempt> {
    let mut guard = clipboard.lock().map_err(|err| lock_err(&err))?;
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
    if let Some(observed) = oversized_payload(max_bytes) {
        let after = pasteboard_sequence();
        drop(guard);
        return Ok(settle(
            &before,
            &after,
            CapturedSnapshot::Oversized {
                sequence: after.clone(),
                observed_bytes: observed,
                limit: max_bytes,
            },
        ));
    }

    // Second pass: load the snapshot. The first pass only rejected
    // the obvious oversize cases; reps that pass it can still grow
    // past `max_bytes` once decoded to UTF-8, and the aggregate
    // of multiple reps is not bounded here at all. The capture
    // loop's post-load `payload_bytes > max_entry_size_bytes`
    // check is the authoritative limit — the first pass just spares
    // us the worst allocations. Mirror `current_snapshot`
    // exactly so the two entry points cannot drift.
    let plain = match guard.get_text() {
        Ok(text) => Some(text),
        Err(arboard::Error::ContentNotAvailable) => None,
        Err(err) => return Err(platform_err(&err)),
    };

    let mut representations = Vec::new();

    #[cfg(target_os = "macos")]
    if let Some(observed) = collect_macos_extras(&mut representations, Some(max_bytes)) {
        let after = pasteboard_sequence();
        drop(guard);
        return Ok(settle(
            &before,
            &after,
            CapturedSnapshot::Oversized {
                sequence: after.clone(),
                observed_bytes: observed,
                limit: max_bytes,
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
