use std::sync::{Arc, Mutex};

use arboard::Clipboard;
use async_trait::async_trait;
#[cfg(target_os = "macos")]
use image::{ImageEncoder, codecs::png::PngEncoder};
#[cfg(target_os = "macos")]
use nagori_core::MAX_DECODED_IMAGE_PIXELS;
#[cfg(target_os = "macos")]
use nagori_core::RepresentationDataRef;
use nagori_core::{
    AppError, ClipboardContent, ClipboardData, ClipboardEntry, ClipboardRepresentation,
    ClipboardSequence, ClipboardSnapshot, Result, StoredClipboardRepresentation,
};
use nagori_platform::{
    CapturedSnapshot, ClipboardReader, ClipboardWriter, SNAPSHOT_CAPTURE_MAX_RETRIES,
    clipboard_blocking, clipboard_write_blocking, has_publishable_representation,
    lock_clipboard_for_write, lock_err, platform_err,
};
#[cfg(target_os = "macos")]
use nagori_platform::{ClipboardExclusionKind, DecodeRgbaError, decode_rgba_with_pixel_cap};
#[cfg(target_os = "macos")]
use objc2::rc::Retained;
#[cfg(target_os = "macos")]
use objc2::runtime::ProtocolObject;
#[cfg(target_os = "macos")]
use objc2_app_kit::{
    NSPasteboard, NSPasteboardItem, NSPasteboardTypeFileURL, NSPasteboardTypeHTML,
    NSPasteboardTypePNG, NSPasteboardTypeRTF, NSPasteboardTypeString, NSPasteboardTypeTIFF,
    NSPasteboardWriting,
};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSArray, NSData, NSString, NSURL};
use time::OffsetDateTime;

#[cfg(target_os = "macos")]
const MAX_FILE_URL_ITEMS: usize = 4096;

/// Hard ceiling on a single pasteboard image representation copied into the
/// daemon's heap.
///
/// The pasteboard owner controls `NSData.length`, and TIFF is deliberately
/// exempt from the `oversized_payload` pre-filter (it is normalised to PNG
/// *before* the entry-size gate so an uncompressed screenshot does not look
/// oversized) — so without this check a malicious owner advertising a
/// multi-GB TIFF would land its full payload in `ns_data_to_vec`. PNG is
/// covered by the pre-filter on the bounded path but not on the unbounded
/// `current_snapshot` path, so both branches check the constant-time
/// `length()` before copying. Mirrors the Linux adapter's
/// `INTERNAL_BODY_CEILING_BYTES` defence-in-depth ceiling: 256 MiB is far
/// above any realistic screenshot or copied image.
#[cfg(target_os = "macos")]
const MAX_IMAGE_REP_BYTES: usize = 256 * 1024 * 1024;

/// UTI mapped to `image/jpeg` for pasteboard publishing.
///
/// `NSPasteboardType` is a `NSString` newtype, so MIMEs that lack a static
/// `NSPasteboardTypePNG`-style constant are still publishable by building a
/// fresh `NSString` from the canonical Uniform Type Identifier. Apps that
/// register `public.jpeg` / `com.compuserve.gif` / `org.webmproject.webp`
/// in their `NSServices` registration (Preview, `TextEdit`, Mail, most
/// browsers) accept these the same way they accept the bundled constants.
#[cfg(target_os = "macos")]
const UTI_JPEG: &str = "public.jpeg";
#[cfg(target_os = "macos")]
const UTI_GIF: &str = "com.compuserve.gif";
#[cfg(target_os = "macos")]
const UTI_WEBP: &str = "org.webmproject.webp";

/// nspasteboard.org marker an owner sets to declare "do not record this in
/// history". A password manager flags a copied secret with
/// [`MARKER_CONCEALED`]; an app that puts a throwaway value on the clipboard
/// flags it with [`MARKER_TRANSIENT`]. We treat both as a hard skip — see
/// [`pasteboard_exclusion`].
#[cfg(target_os = "macos")]
const MARKER_CONCEALED: &str = "org.nspasteboard.ConcealedType";
#[cfg(target_os = "macos")]
const MARKER_TRANSIENT: &str = "org.nspasteboard.TransientType";

/// macOS clipboard adapter.
///
/// The `Arc<Mutex<Clipboard>>` is **not** there for arboard's internal
/// thread-safety alone — `Clipboard::get_text` / `set_text` already take
/// `&mut self`, so exclusive access would be required regardless. The lock
/// extends across `get_text` + `collect_macos_extras` (and the `dataForType`
/// loads in `current_snapshot_with_max`) so a concurrent `write_image_bytes`
/// cannot slip its `clearContents`/`setData` pair between the two and stitch
/// a torn snapshot.
pub struct MacosClipboard {
    clipboard: Arc<Mutex<Clipboard>>,
}

impl MacosClipboard {
    pub fn new() -> Result<Self> {
        Ok(Self {
            clipboard: Arc::new(Mutex::new(
                Clipboard::new().map_err(|err| platform_err(&err))?,
            )),
        })
    }
}

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
enum CaptureAttempt {
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
fn resolve_capture_attempts(
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
fn settle(
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

#[async_trait]
impl ClipboardWriter for MacosClipboard {
    async fn write_entry(&self, entry: &ClipboardEntry) -> Result<()> {
        if let ClipboardContent::Image(image) = &entry.content {
            let bytes = image.pending_bytes.clone().ok_or_else(|| {
                AppError::Platform(
                    "image payload bytes were not loaded before clipboard write".to_owned(),
                )
            })?;
            let mime = image
                .mime_type
                .clone()
                .unwrap_or_else(|| "image/png".to_owned());
            return self.write_image_bytes(bytes, &mime).await;
        }
        if let ClipboardContent::FileList(files) = &entry.content {
            // Republish the stored POSIX paths as `NSPasteboardTypeFileURL`
            // pasteboard items so Finder accepts a file paste. Without this
            // branch the entry fell through to `plain_text()` and pasted the
            // paths as text — which never lands in Finder as files.
            return self.write_files(files.paths.clone()).await;
        }
        let Some(text) = entry.plain_text() else {
            return Err(AppError::Unsupported(
                "clipboard entry has no representable payload".to_owned(),
            ));
        };
        self.write_text(text).await
    }

    async fn write_plain(&self, entry: &ClipboardEntry) -> Result<()> {
        let Some(text) = entry.plain_text() else {
            return Err(AppError::Unsupported(
                "clipboard entry has no plain-text payload".to_owned(),
            ));
        };
        self.write_text(text).await
    }

    async fn write_text(&self, text: &str) -> Result<()> {
        let clipboard = self.clipboard.clone();
        let owned = text.to_owned();
        clipboard_write_blocking("write_text", move || -> Result<()> {
            // Bounded lock acquisition (no OS side effect yet) so a guard
            // leaked by a timed-out read cannot park this write forever;
            // the `set_text` itself still runs to completion unbounded.
            lock_clipboard_for_write(&clipboard, "write_text")?
                .set_text(owned)
                .map_err(|err| platform_err(&err))
        })
        .await
        .map_err(|err| AppError::Platform(err.to_string()))?
    }

    async fn write_representations(
        &self,
        entry: &ClipboardEntry,
        representations: &[StoredClipboardRepresentation],
    ) -> Result<()> {
        // Pre-scan before touching the pasteboard so an entry whose stored
        // representations all sit outside the macOS publisher's MIME table
        // (file URLs today, or images saved as `image/jpeg` / `image/gif`)
        // falls back to `write_entry` instead of leaving the user with a
        // cleared pasteboard and a copy error. Doing the scan up here keeps
        // the responsibility on the adapter that knows its own mapping
        // table; the daemon stays oblivious to which MIMEs are publishable.
        if representations.is_empty() || !has_publishable_representation(representations) {
            return self.write_entry(entry).await;
        }
        self.publish_representations(representations.to_vec()).await
    }

    async fn write_representation_exact(
        &self,
        representation: &StoredClipboardRepresentation,
    ) -> Result<()> {
        // Strict single-representation paste: refuse a MIME this adapter
        // cannot publish instead of falling back to the primary the way
        // `write_representations` does. `publish_representations` builds the
        // pasteboard item off-pasteboard and only clears + writes when at
        // least one item maps, so an unmapped rep leaves the clipboard
        // untouched rather than blanking it.
        if !has_publishable_representation(std::slice::from_ref(representation)) {
            return Err(AppError::Unsupported(
                "representation cannot be published to the macOS clipboard".to_owned(),
            ));
        }
        self.publish_representations(vec![representation.clone()])
            .await
    }
}

impl MacosClipboard {
    #[cfg(target_os = "macos")]
    async fn write_image_bytes(&self, bytes: Vec<u8>, mime: &str) -> Result<()> {
        let mime_owned = mime.to_owned();
        let clipboard = self.clipboard.clone();
        clipboard_write_blocking("write_image_bytes", move || -> Result<()> {
            // Take the same arboard mutex `current_snapshot` and the text
            // path use so a concurrent reader/writer cannot race the
            // clearContents+setData pair below on the shared NSPasteboard.
            // Acquisition is bounded (see `lock_clipboard_for_write`) so a
            // guard leaked by a timed-out read cannot park the write.
            let _guard = lock_clipboard_for_write(&clipboard, "write_image_bytes")?;
            // Drain the AppKit autoreleased temporaries (`generalPasteboard`,
            // the `NSData` copy, dynamic-UTI `NSString`s) on every call. The
            // capture/copy work runs on a tokio blocking-pool thread with no
            // implicit pool, so without this each image copy would leak its
            // temporaries into a pool that never drains — matching the read
            // side (`current_snapshot` / `oversized_payload`).
            objc2::rc::autoreleasepool(|_pool| {
                // SAFETY: AppKit FFI calls into the shared NSPasteboard.
                // - `NSPasteboardTypePNG`/`NSPasteboardTypeTIFF` are static
                //   extern constants whose backing storage lives for the
                //   lifetime of the AppKit framework.
                // - `NSData::with_bytes` copies our `&bytes` slice into a
                //   fresh ObjC heap buffer, so the resulting `Retained<NSData>`
                //   does not depend on any Rust lifetime once it leaves this
                //   call. Handing it to `setData_forType` only requires a
                //   shared reference, and AppKit retains its own copy of the
                //   data on the pasteboard before returning.
                unsafe {
                    // Dynamic UTI strings (JPEG/GIF/WebP) outlive the call by
                    // virtue of `Retained<NSString>` keeping the backing buffer
                    // alive across `setData_forType` — AppKit copies the type
                    // string before returning. Static `NSPasteboardType*` constants
                    // are handed straight back to AppKit by reference.
                    //
                    // Resolve the pasteboard type and build the NSData *before*
                    // `clearContents` so an unsupported MIME (or a failed NSData
                    // construction) leaves the user's clipboard intact instead of
                    // blanking it and then erroring — the same "build and
                    // validate before clearing" contract `publish_representations`
                    // follows for the multi-rep path.
                    let dynamic_ty: Retained<NSString>;
                    let ty: &NSString = match mime_owned.as_str() {
                        "image/png" => NSPasteboardTypePNG,
                        "image/tiff" => NSPasteboardTypeTIFF,
                        "image/jpeg" => {
                            dynamic_ty = NSString::from_str(UTI_JPEG);
                            &dynamic_ty
                        }
                        "image/gif" => {
                            dynamic_ty = NSString::from_str(UTI_GIF);
                            &dynamic_ty
                        }
                        "image/webp" => {
                            dynamic_ty = NSString::from_str(UTI_WEBP);
                            &dynamic_ty
                        }
                        other => {
                            return Err(AppError::Unsupported(format!(
                                "unsupported image clipboard mime type: {other}"
                            )));
                        }
                    };
                    let data = NSData::with_bytes(&bytes);
                    let pb = NSPasteboard::generalPasteboard();
                    pb.clearContents();
                    if !pb.setData_forType(Some(&data), ty) {
                        return Err(AppError::Platform(
                            "NSPasteboard::setData failed for image type".to_owned(),
                        ));
                    }
                    Ok(())
                }
            })
        })
        .await
        .map_err(|err| AppError::Platform(err.to_string()))?
    }

    // Keep this async so the cfg-neutral caller can await both platform variants.
    #[cfg(not(target_os = "macos"))]
    #[allow(clippy::unused_async)]
    async fn write_image_bytes(&self, _bytes: Vec<u8>, _mime: &str) -> Result<()> {
        Err(AppError::Unsupported(
            "image clipboard writes are macOS-only".to_owned(),
        ))
    }

    #[cfg(target_os = "macos")]
    async fn write_files(&self, paths: Vec<String>) -> Result<()> {
        // A copy-back of a zero-path file list would blank the pasteboard with
        // an empty offer that downstream readers surface as "empty file list".
        // Refuse it up front so the caller keeps the previous clipboard rather
        // than clearing it for nothing — mirrors the Linux adapter's
        // `write_files` contract.
        if paths.is_empty() {
            return Err(AppError::Unsupported(
                "file-list clipboard entry has no paths".to_owned(),
            ));
        }
        let clipboard = self.clipboard.clone();
        clipboard_write_blocking("write_files", move || -> Result<()> {
            // Hold the arboard mutex across `clearContents` + `writeObjects`
            // so a concurrent reader cannot observe the cleared-but-not-yet-
            // written window, matching `write_image_bytes` /
            // `publish_representations`. Acquisition is bounded so a guard
            // leaked by a timed-out read cannot park the write.
            let _guard = lock_clipboard_for_write(&clipboard, "write_files")?;
            // Drain AppKit autoreleased temporaries (`generalPasteboard`, the
            // per-path `NSPasteboardItem` / `NSString`s) on every call: the
            // copy work runs on a tokio blocking-pool thread with no implicit
            // pool, so without this each file-list copy would leak them.
            objc2::rc::autoreleasepool(|_pool| -> Result<()> {
                let items = file_url_pasteboard_items(&paths);
                if items.is_empty() {
                    // Every path failed `url::Url::from_file_path` (all
                    // relative / malformed, e.g. a corrupted history row).
                    // Leave the pasteboard untouched and surface the failure.
                    return Err(AppError::Platform(
                        "no stored file path could be represented as a file URL".to_owned(),
                    ));
                }
                let pb = NSPasteboard::generalPasteboard();
                pb.clearContents();
                if write_pasteboard_items(&pb, items) {
                    Ok(())
                } else {
                    Err(AppError::Platform(
                        "NSPasteboard rejected the file-list writeObjects batch".to_owned(),
                    ))
                }
            })
        })
        .await
        .map_err(|err| AppError::Platform(err.to_string()))?
    }

    // Keep this async so the cfg-neutral caller can await both platform variants.
    #[cfg(not(target_os = "macos"))]
    #[allow(clippy::unused_async)]
    async fn write_files(&self, _paths: Vec<String>) -> Result<()> {
        Err(AppError::Unsupported(
            "file-list clipboard writes are macOS-only".to_owned(),
        ))
    }

    #[cfg(target_os = "macos")]
    async fn publish_representations(
        &self,
        representations: Vec<StoredClipboardRepresentation>,
    ) -> Result<()> {
        let clipboard = self.clipboard.clone();
        clipboard_write_blocking("publish_representations", move || -> Result<()> {
            // Hold the arboard mutex across the whole clearContents +
            // writeObjects batch so a concurrent reader cannot observe a
            // partial state with the primary published but the plain
            // fallback still missing. Acquisition is bounded so a guard
            // leaked by a timed-out read cannot park the write.
            let _guard = lock_clipboard_for_write(&clipboard, "publish_representations")?;
            objc2::rc::autoreleasepool(|_pool| -> Result<()> {
                // Build every pasteboard item off-pasteboard first, then
                // publish the whole batch atomically with `writeObjects`.
                // Inline reps (text / HTML / RTF / image) share one
                // `NSPasteboardItem` — one value per type — while a
                // `text/uri-list` rep fans out to one item per file URL so a
                // multi-file copy-back keeps every path instead of collapsing
                // to the last URL the legacy `setString_forType` loop kept.
                let PreparedRepresentations { items, published } =
                    prepare_representation_items(&representations);
                if published == 0 {
                    // Pre-scan in `write_representations` rules this out in
                    // normal use; the only way to reach it now is if every
                    // mapped rep was rejected by AppKit while building its
                    // item. The pasteboard is still intact — we have not
                    // cleared it — so surface the platform error and leave
                    // the user's clipboard untouched rather than blanking it.
                    return Err(AppError::Platform(
                        "NSPasteboard rejected every mapped representation".to_owned(),
                    ));
                }
                let pb = NSPasteboard::generalPasteboard();
                pb.clearContents();
                if write_pasteboard_items(&pb, items) {
                    Ok(())
                } else {
                    Err(AppError::Platform(
                        "NSPasteboard rejected the representation writeObjects batch".to_owned(),
                    ))
                }
            })
        })
        .await
        .map_err(|err| AppError::Platform(err.to_string()))?
    }

    #[cfg(not(target_os = "macos"))]
    #[allow(clippy::unused_async)]
    async fn publish_representations(
        &self,
        _representations: Vec<StoredClipboardRepresentation>,
    ) -> Result<()> {
        Err(AppError::Unsupported(
            "the macOS clipboard adapter only runs on a macOS host".to_owned(),
        ))
    }
}

/// Result of mapping one inline stored representation onto an
/// `NSPasteboardItem`.
///
/// `Skipped` and `Failed` are kept distinct so the caller can warn about a
/// MIME we promised to publish but `AppKit` rejected, without spamming a warn
/// for a rep with no `NSPasteboardType` mapping (that is a `Skipped`, not a
/// bug). `text/uri-list` reps never reach this enum — they fan out to one
/// item per file URL in [`prepare_representation_items`].
#[cfg(target_os = "macos")]
enum PublishOutcome {
    /// Mapped to a known `NSPasteboardType` and `AppKit` accepted the bytes.
    Published,
    /// No `NSPasteboardType` mapping; nothing was attempted on `AppKit`.
    Skipped,
    /// Mapped to a known `NSPasteboardType` but `setString` / `setData`
    /// returned `NO` — exceptional, the caller surfaces it at warn.
    Failed,
}

/// The pasteboard items built for a multi-rep `writeObjects` batch, plus
/// the count of stored reps that actually mapped onto an item.
///
/// `published` lets the caller distinguish "nothing landed, leave the
/// pasteboard intact" from "at least one rep is publishable, clear and
/// write" without re-walking the items: a single inline-data item can carry
/// several reps (text + HTML + image), and a `text/uri-list` rep fans out to
/// many items, so the item count alone is not the rep count.
#[cfg(target_os = "macos")]
struct PreparedRepresentations {
    items: Vec<Retained<NSPasteboardItem>>,
    published: usize,
}

/// Build the `NSPasteboardItem` batch for a stored representation set.
///
/// Inline reps (text/plain, text/html, application/rtf, and the image MIMEs)
/// share a single `NSPasteboardItem` — one value per type, exactly how a rich
/// single clip is modelled — so a paste target that wants HTML still sees it
/// alongside the plain-text fallback. A `text/uri-list` rep fans out to one
/// `NSPasteboardItem` per file URL, which is the Apple-documented way to put
/// multiple files on the pasteboard; this is what fixes the old
/// "multi-file list collapses to its last URL" limitation. The inline-data
/// item is ordered first so a text-only paste target reads it as the primary
/// item; in practice file reps and inline reps do not co-occur, so the order
/// only matters for the degenerate mixed case.
#[cfg(target_os = "macos")]
fn prepare_representation_items(reps: &[StoredClipboardRepresentation]) -> PreparedRepresentations {
    let data_item = NSPasteboardItem::new();
    let mut data_item_types = 0_usize;
    let mut file_items: Vec<Retained<NSPasteboardItem>> = Vec::new();
    let mut published = 0_usize;

    for rep in reps {
        if let ("text/uri-list", RepresentationDataRef::FilePaths(paths)) =
            (rep.mime_type.as_str(), &rep.data)
        {
            for path in paths {
                let Some(item) = file_url_pasteboard_item(path) else {
                    continue;
                };
                file_items.push(item);
                published = published.saturating_add(1);
            }
            continue;
        }
        match publish_inline_representation(&data_item, rep) {
            PublishOutcome::Published => {
                data_item_types = data_item_types.saturating_add(1);
                published = published.saturating_add(1);
            }
            PublishOutcome::Skipped => {}
            PublishOutcome::Failed => {
                // AppKit accepted the mapping but `setString` / `setData`
                // returned NO — keep going so the rest of the rep set still
                // lands, but surface the failure at warn so a primary HTML /
                // image drop is visible in logs instead of being hidden
                // behind the surviving plain fallback.
                tracing::warn!(
                    mime = %rep.mime_type,
                    role = ?rep.role,
                    ordinal = rep.ordinal,
                    "NSPasteboardItem rejected setString/setData for stored representation",
                );
            }
        }
    }

    let mut items = Vec::with_capacity(file_items.len() + 1);
    if data_item_types > 0 {
        items.push(data_item);
    }
    items.extend(file_items);
    PreparedRepresentations { items, published }
}

/// Map one inline (non-file) stored representation onto `item`.
///
/// The MIME → `NSPasteboardType` table covers plain text, HTML, RTF, PNG,
/// TIFF, JPEG, GIF, and WebP. Image MIMEs that lack a static
/// `NSPasteboardType*` constant (JPEG/GIF/WebP) are published against their
/// canonical Uniform Type Identifier strings — `AppKit` copies the type name,
/// so the dynamic `NSString` only has to outlive `setData_forType`.
/// `text/uri-list` reps are handled by the caller's per-file fan-out, so they
/// fall through to `Skipped` here.
#[cfg(target_os = "macos")]
fn publish_inline_representation(
    item: &NSPasteboardItem,
    rep: &StoredClipboardRepresentation,
) -> PublishOutcome {
    let accepted = match (rep.mime_type.as_str(), &rep.data) {
        ("text/plain", RepresentationDataRef::InlineText(text)) => {
            let value = NSString::from_str(text);
            // SAFETY: `NSPasteboardTypeString` is a `'static` AppKit constant
            // and `value` is a freshly retained NSString copied by the item.
            item.setString_forType(&value, unsafe { NSPasteboardTypeString })
        }
        ("text/html", RepresentationDataRef::InlineText(text)) => {
            let value = NSString::from_str(text);
            item.setString_forType(&value, unsafe { NSPasteboardTypeHTML })
        }
        ("application/rtf", RepresentationDataRef::InlineText(text)) => {
            let value = NSString::from_str(text);
            item.setString_forType(&value, unsafe { NSPasteboardTypeRTF })
        }
        ("image/png", RepresentationDataRef::DatabaseBlob(bytes)) => {
            let data = NSData::with_bytes(bytes);
            item.setData_forType(&data, unsafe { NSPasteboardTypePNG })
        }
        ("image/tiff", RepresentationDataRef::DatabaseBlob(bytes)) => {
            let data = NSData::with_bytes(bytes);
            item.setData_forType(&data, unsafe { NSPasteboardTypeTIFF })
        }
        ("image/jpeg", RepresentationDataRef::DatabaseBlob(bytes)) => {
            let data = NSData::with_bytes(bytes);
            let ty = NSString::from_str(UTI_JPEG);
            item.setData_forType(&data, &ty)
        }
        ("image/gif", RepresentationDataRef::DatabaseBlob(bytes)) => {
            let data = NSData::with_bytes(bytes);
            let ty = NSString::from_str(UTI_GIF);
            item.setData_forType(&data, &ty)
        }
        ("image/webp", RepresentationDataRef::DatabaseBlob(bytes)) => {
            let data = NSData::with_bytes(bytes);
            let ty = NSString::from_str(UTI_WEBP);
            item.setData_forType(&data, &ty)
        }
        (mime, _) => {
            tracing::debug!(
                mime = %mime,
                role = ?rep.role,
                ordinal = rep.ordinal,
                "skipping representation without a NSPasteboard mapping",
            );
            return PublishOutcome::Skipped;
        }
    };
    if accepted {
        PublishOutcome::Published
    } else {
        PublishOutcome::Failed
    }
}

/// Build one `NSPasteboardItem` carrying a single file's `file://` URL.
///
/// Returns `None` when the stored path is not representable as a file URL
/// (relative / malformed entry from a corrupted history row) or `AppKit`
/// rejects the `setString` — the caller skips it so a single bad path does
/// not abort the rest of the file list.
#[cfg(target_os = "macos")]
fn file_url_pasteboard_item(path: &str) -> Option<Retained<NSPasteboardItem>> {
    let url = path_to_file_url(path)?;
    let item = NSPasteboardItem::new();
    let value = NSString::from_str(&url);
    // SAFETY: `NSPasteboardTypeFileURL` is a `'static` AppKit constant and
    // `value` is a freshly retained NSString copied by the item.
    if item.setString_forType(&value, unsafe { NSPasteboardTypeFileURL }) {
        Some(item)
    } else {
        tracing::warn!(%path, "NSPasteboardItem rejected file URL");
        None
    }
}

/// Build the `NSPasteboardItem`s for a list of file paths.
///
/// Drops paths that cannot be turned into a file URL (logged per-path by
/// [`file_url_pasteboard_item`]); the caller treats an all-dropped result as
/// a publish failure rather than clearing the pasteboard for nothing.
#[cfg(target_os = "macos")]
fn file_url_pasteboard_items(paths: &[String]) -> Vec<Retained<NSPasteboardItem>> {
    paths
        .iter()
        .filter_map(|p| file_url_pasteboard_item(p))
        .collect()
}

/// Clear-then-`writeObjects` the prepared items onto the shared pasteboard.
///
/// `NSPasteboardItem` conforms to `NSPasteboardWriting`, so the batch is
/// type-erased through `ProtocolObject` and handed to `writeObjects` in one
/// transaction. Returns the `writeObjects` success flag. The caller holds the
/// arboard mutex and has already drained the autorelease pool, and is
/// responsible for the preceding `clearContents`.
#[cfg(target_os = "macos")]
fn write_pasteboard_items(pb: &NSPasteboard, items: Vec<Retained<NSPasteboardItem>>) -> bool {
    let writers: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = items
        .into_iter()
        .map(ProtocolObject::from_retained)
        .collect();
    let array = NSArray::from_retained_slice(&writers);
    pb.writeObjects(&array)
}

/// Convert a filesystem path string into a `file://` URL string suitable
/// for `NSPasteboardTypeFileURL`. Returns `None` for paths that aren't
/// representable as a URL — typically relative paths from a corrupted
/// history row.
#[cfg(target_os = "macos")]
fn path_to_file_url(path: &str) -> Option<String> {
    let trimmed = std::path::Path::new(path);
    url::Url::from_file_path(trimmed)
        .ok()
        .map(|u| u.to_string())
}

#[cfg(target_os = "macos")]
fn pasteboard_sequence() -> ClipboardSequence {
    // Drain any AppKit autoreleased temporaries (NSPasteboard return value,
    // intermediate objects from `+generalPasteboard`) at the end of this call
    // so the daemon's long-running blocking-pool thread does not accumulate
    // them across thousands of polls.
    objc2::rc::autoreleasepool(|_pool| {
        let pb = NSPasteboard::generalPasteboard();
        // NSInteger fits in i64 on every supported architecture; the change
        // counter is monotonically increasing across the process lifetime so
        // wraparound is theoretical.
        let count = pb.changeCount() as i64;
        ClipboardSequence::native(count)
    })
}

#[cfg(not(target_os = "macos"))]
const fn pasteboard_sequence() -> ClipboardSequence {
    ClipboardSequence::unsupported()
}

#[cfg(target_os = "macos")]
fn collect_macos_extras(
    out: &mut Vec<ClipboardRepresentation>,
    max_file_url_bytes: Option<usize>,
) -> Option<usize> {
    // Wrap the AppKit reads in an explicit autorelease pool. AppKit's
    // `dataForType` / `stringForType` return autoreleased temporaries; the
    // capture loop runs on a tokio blocking-pool thread that has no implicit
    // pool of its own, so without this each poll would leak the returned
    // NSData/NSString into a pool that never drains.
    objc2::rc::autoreleasepool(|_pool| {
        // SAFETY: the methods invoked below are FFI calls into AppKit. They are
        // documented to return optional `NSString`/`NSData` values without side
        // effects on the running process, and we never retain the returned
        // pointers past the borrow returned by `Retained`.
        unsafe {
            let pb = NSPasteboard::generalPasteboard();

            // File URLs come through per-pasteboard-item; the pasteboard-level
            // accessors only return the first one.
            if let Some(items) = pb.pasteboardItems() {
                match collect_file_url_paths(&items, max_file_url_bytes) {
                    FileUrlPaths::Captured(paths) => {
                        if !paths.is_empty() {
                            out.push(ClipboardRepresentation {
                                mime_type: "text/uri-list".to_owned(),
                                data: ClipboardData::FilePaths(paths),
                            });
                        }
                    }
                    // `collect_file_url_paths` returns as soon as the byte
                    // budget or the unconditional `MAX_FILE_URL_ITEMS` count cap
                    // trips, so memory is already bounded by the time we get
                    // here. What differs is how an oversized file-list should be
                    // surfaced:
                    // - bounded (`Some`) path: the whole snapshot is rejected,
                    //   so propagate `observed` and let the caller turn it into
                    //   `CapturedSnapshot::Oversized`.
                    // - unbounded (`None`) `current_snapshot` path: there is no
                    //   budget to reject against, so drop just the oversized
                    //   file-list and keep collecting the remaining extras
                    //   (HTML / RTF / image) rather than silently losing them.
                    FileUrlPaths::Oversized(observed) => {
                        if max_file_url_bytes.is_some() {
                            return Some(observed);
                        }
                    }
                }
            }

            if let Some(html) = pb.stringForType(NSPasteboardTypeHTML) {
                out.push(ClipboardRepresentation {
                    mime_type: "text/html".to_owned(),
                    data: ClipboardData::Text(html.to_string()),
                });
            }

            if let Some(rtf) = pb.stringForType(NSPasteboardTypeRTF) {
                out.push(ClipboardRepresentation {
                    mime_type: "application/rtf".to_owned(),
                    data: ClipboardData::Text(rtf.to_string()),
                });
            }

            // Prefer PNG when both PNG and TIFF are present. macOS screenshot
            // shortcuts commonly publish TIFF only; that gets normalised to
            // PNG after this timed read (see `transcode_tiff_representations`)
            // so the uncompressed pasteboard form does not make ordinary
            // screenshots look oversized against the entry-size gate. Both
            // branches probe the constant-time `length()` against
            // `MAX_IMAGE_REP_BYTES` before `ns_data_to_vec` copies the
            // payload — see the constant's doc for why TIFF has no other
            // allocation bound.
            if let Some(data) = pb.dataForType(NSPasteboardTypePNG)
                && image_rep_within_ceiling("image/png", &data)
                && let Some(bytes) = ns_data_to_vec(&data)
            {
                out.push(ClipboardRepresentation {
                    mime_type: "image/png".to_owned(),
                    data: ClipboardData::Bytes(bytes),
                });
            } else if let Some(data) = pb.dataForType(NSPasteboardTypeTIFF)
                && image_rep_within_ceiling("image/tiff", &data)
                && let Some(bytes) = ns_data_to_vec(&data)
            {
                // Emit the *raw* TIFF here. The CPU-bound TIFF->PNG
                // normalisation runs outside the clipboard-read timeout (see
                // `transcode_tiff_representations`); only the raw-byte copy —
                // already bounded by `image_rep_within_ceiling` — stays under
                // the pasteboard lock. The transcoded image's size budget is
                // re-applied off the timed path (`finalize_captured`), so the
                // `max_file_url_bytes` oversize check moves there too.
                out.push(ClipboardRepresentation {
                    mime_type: "image/tiff".to_owned(),
                    data: ClipboardData::Bytes(bytes),
                });
            }
        }
        None
    })
}

/// Pre-copy admission check for a pasteboard image representation: `true`
/// when `data` fits under [`MAX_IMAGE_REP_BYTES`]. The over-ceiling case is
/// logged (length only, never content) and the representation is skipped —
/// the rest of the snapshot still flows through, matching how an
/// undecodable TIFF is dropped.
#[cfg(target_os = "macos")]
fn image_rep_within_ceiling(mime: &str, data: &objc2_foundation::NSData) -> bool {
    let byte_count = data.length();
    if byte_count > MAX_IMAGE_REP_BYTES {
        tracing::warn!(
            mime,
            byte_count,
            ceiling = MAX_IMAGE_REP_BYTES,
            "pasteboard_image_rep_exceeds_ceiling"
        );
        return false;
    }
    true
}

/// Normalise a captured TIFF to PNG, guarded by the shared decoded-pixel
/// cap.
///
/// `decode_rgba_with_pixel_cap` probes the dimensions before `to_rgba8` —
/// without that a 65535×65535 TIFF would force a multi-GB allocation well
/// before the snapshot's byte-budget check runs. Drop the image rep
/// entirely (rest of the snapshot still flows through) when:
///   * dimensions exceed `MAX_DECODED_IMAGE_PIXELS`, or
///   * dimensions are unreadable — `image` could not sniff the TIFF
///     header, so a subsequent `decode()` would not succeed either and
///     saving an opaque blob serves no UI purpose.
///
/// A decode or PNG-encode failure *after* a readable header keeps the
/// original TIFF bytes instead: the payload is well-formed enough to show
/// dimensions, so storing it still serves copy-back even if this host could
/// not transcode it.
#[cfg(target_os = "macos")]
fn prepare_tiff_capture(bytes: Vec<u8>) -> Option<(String, Vec<u8>)> {
    let rgba = match decode_rgba_with_pixel_cap(&bytes, MAX_DECODED_IMAGE_PIXELS) {
        Ok(rgba) => rgba,
        Err(DecodeRgbaError::DimensionsUnreadable { .. }) => {
            tracing::warn!(
                byte_count = bytes.len(),
                "tiff_capture_dropped reason=dimensions_unreadable"
            );
            return None;
        }
        Err(DecodeRgbaError::PixelCapExceeded { pixels, max_pixels }) => {
            tracing::warn!(
                pixels,
                max_pixels,
                "tiff_capture_dropped reason=decoded_pixels_exceed_cap"
            );
            return None;
        }
        Err(DecodeRgbaError::DecodeFailed { .. }) => {
            tracing::warn!(
                byte_count = bytes.len(),
                "tiff_to_png_failed_using_original"
            );
            return Some(("image/tiff".to_owned(), bytes));
        }
    };
    let (width, height) = rgba.dimensions();
    let mut png = Vec::new();
    if PngEncoder::new(&mut png)
        .write_image(&rgba, width, height, image::ExtendedColorType::Rgba8)
        .is_ok()
    {
        Some(("image/png".to_owned(), png))
    } else {
        tracing::warn!(
            byte_count = bytes.len(),
            "tiff_to_png_failed_using_original"
        );
        Some(("image/tiff".to_owned(), bytes))
    }
}

/// Replace any captured `image/tiff` representation with its PNG
/// normalisation, dropping it when the TIFF is undecodable / over the pixel
/// cap (`prepare_tiff_capture` returns `None`).
///
/// Runs **outside** [`CLIPBOARD_OP_TIMEOUT`]: the TIFF decode + PNG
/// re-encode is CPU-bound (bounded by `MAX_DECODED_IMAGE_PIXELS`), touches
/// neither the pasteboard nor the arboard mutex, and so is not the OS hang
/// the read timeout guards against. Running it inside the timed read made a
/// legitimately large screenshot — one that passes the 64-megapixel cap but
/// whose transcode exceeds 3s — time out *permanently*, and pinned the
/// leaked blocking thread's mutex against later writes. The raw bytes are
/// already captured and torn-checked under the lock, so transcoding the
/// owned buffer here is safe. Mirrors the write path's `write_image_bytes`,
/// which already decodes off the timed section.
#[cfg(target_os = "macos")]
fn transcode_tiff_representations(
    representations: Vec<ClipboardRepresentation>,
) -> Vec<ClipboardRepresentation> {
    representations
        .into_iter()
        .filter_map(|rep| match rep {
            ClipboardRepresentation {
                mime_type,
                data: ClipboardData::Bytes(bytes),
            } if mime_type == "image/tiff" => {
                prepare_tiff_capture(bytes).map(|(mime_type, bytes)| ClipboardRepresentation {
                    mime_type,
                    data: ClipboardData::Bytes(bytes),
                })
            }
            other => Some(other),
        })
        .collect()
}

/// Run [`transcode_tiff_representations`] on the blocking pool, *without*
/// the read timeout.
#[cfg(target_os = "macos")]
async fn transcode_representations(
    representations: Vec<ClipboardRepresentation>,
) -> Result<Vec<ClipboardRepresentation>> {
    tokio::task::spawn_blocking(move || transcode_tiff_representations(representations))
        .await
        .map_err(|err| AppError::Platform(err.to_string()))
}

/// Normalise a `current_snapshot` result after the timed read returns.
#[cfg(target_os = "macos")]
async fn transcode_snapshot(mut snapshot: ClipboardSnapshot) -> Result<ClipboardSnapshot> {
    snapshot.representations = transcode_representations(snapshot.representations).await?;
    Ok(snapshot)
}

#[cfg(not(target_os = "macos"))]
#[allow(clippy::unused_async)]
async fn transcode_snapshot(snapshot: ClipboardSnapshot) -> Result<ClipboardSnapshot> {
    Ok(snapshot)
}

/// Normalise a bounded capture after the timed read returns, then re-apply
/// the entry-size budget to the transcoded image.
///
/// The raw pasteboard probe (`oversized_payload`) never sizes TIFF — only
/// PNG / HTML / RTF / text / file URLs — so the normalised image size is
/// first known here. Surfacing it as `Oversized` keeps the pre-read drop
/// semantics the in-timed transcode used to enforce.
#[cfg(target_os = "macos")]
async fn finalize_captured(
    captured: CapturedSnapshot,
    max_bytes: usize,
) -> Result<CapturedSnapshot> {
    let CapturedSnapshot::Captured(mut snapshot) = captured else {
        // `Oversized` was already decided on raw pasteboard sizes, and
        // `Excluded` skipped the body read entirely — neither has anything to
        // transcode, so pass them through untouched.
        return Ok(captured);
    };
    snapshot.representations = transcode_representations(snapshot.representations).await?;
    if let Some(observed) = snapshot
        .representations
        .iter()
        .find_map(|rep| match &rep.data {
            ClipboardData::Bytes(bytes) if bytes.len() > max_bytes => Some(bytes.len()),
            _ => None,
        })
    {
        return Ok(CapturedSnapshot::Oversized {
            sequence: snapshot.sequence,
            observed_bytes: observed,
            limit: max_bytes,
        });
    }
    Ok(CapturedSnapshot::Captured(snapshot))
}

#[cfg(not(target_os = "macos"))]
#[allow(clippy::unused_async)]
async fn finalize_captured(
    captured: CapturedSnapshot,
    _max_bytes: usize,
) -> Result<CapturedSnapshot> {
    Ok(captured)
}

#[cfg(target_os = "macos")]
enum FileUrlPaths {
    Captured(Vec<String>),
    Oversized(usize),
}

#[cfg(target_os = "macos")]
fn collect_file_url_paths(
    items: &NSArray<NSPasteboardItem>,
    max_bytes: Option<usize>,
) -> FileUrlPaths {
    let mut paths = Vec::new();
    let mut observed_bytes = 0_usize;
    let mut file_url_count = 0_usize;

    for item in items {
        // SAFETY: `NSPasteboardTypeFileURL` is a static AppKit pasteboard type
        // constant with framework lifetime.
        let Some(string) = item.stringForType(unsafe { NSPasteboardTypeFileURL }) else {
            continue;
        };
        file_url_count = file_url_count.saturating_add(1);
        observed_bytes = observed_bytes.saturating_add(string.len());

        // Cap the item count unconditionally — including the unbounded
        // (`max_bytes == None`) `current_snapshot` path — so a pasteboard
        // advertising millions of file URLs cannot grow `paths` without bound.
        // Mirrors Windows' unconditional `MAX_PATHS` cap; the byte-budget check
        // below still only runs when a caller passes a limit.
        if file_url_count > MAX_FILE_URL_ITEMS {
            let observed = max_bytes.map_or(observed_bytes, |limit| {
                observed_bytes.max(limit_exceeded_bytes(limit))
            });
            return FileUrlPaths::Oversized(observed);
        }
        if let Some(limit) = max_bytes
            && observed_bytes > limit
        {
            return FileUrlPaths::Oversized(observed_bytes);
        }

        // Finder frequently lands a *file reference URL* on the pasteboard;
        // resolve it to a path-based URL before decoding so the palette shows
        // the real path instead of the `/.file/id=…` handle. An unresolvable
        // reference URL is dropped so the handle never leaks to the palette.
        let raw = match resolve_file_url(&string) {
            FileUrlResolution::Resolved(resolved) => resolved,
            FileUrlResolution::Raw => string.to_string(),
            FileUrlResolution::Drop => continue,
        };
        if let Some(path) = file_url_to_path(&raw) {
            paths.push(path);
        }
    }

    FileUrlPaths::Captured(paths)
}

/// Detect an owner-declared "do not record this" marker on the general
/// pasteboard, returning the [`ClipboardExclusionKind`] when present.
///
/// `availableTypeFromArray` returns the first candidate type the pasteboard
/// offers, so listing `Concealed` before `Transient` fixes the priority the
/// capture loop relies on: when an owner sets both markers, the concealed
/// (secret) signal wins. Only the marker's *presence* matters — we never ask
/// for its data — so the secret body is never pulled into our address space.
/// This runs before the `oversized_payload` probe and the `get_text` body
/// read in [`capture_attempt`], mirroring how the capture loop skips a secure
/// focus before reading the clipboard.
#[cfg(target_os = "macos")]
fn pasteboard_exclusion() -> Option<ClipboardExclusionKind> {
    // Same autoreleasepool discipline as `oversized_payload` /
    // `pasteboard_sequence`: drain the AppKit temporaries (`+generalPasteboard`,
    // the candidate NSStrings, the returned type) on every call so the
    // blocking-pool thread does not accumulate them across polls.
    objc2::rc::autoreleasepool(|_pool| exclusion_for(&NSPasteboard::generalPasteboard()))
}

/// Marker test for a specific pasteboard. Split out from
/// [`pasteboard_exclusion`] so a unit test can exercise the detection and
/// `Concealed`-priority logic against an isolated `pasteboardWithUniqueName`
/// rather than clobbering the shared general pasteboard.
///
/// `availableTypeFromArray` returns the first candidate the receiver offers,
/// so listing `Concealed` before `Transient` makes a concealed secret win
/// when an owner sets both. It is a presence test on the receiver's declared
/// types and is unrelated to the `NSPasteboard` Filter Services that convert
/// between known UTIs, so it never spuriously reports an opaque marker type.
#[cfg(target_os = "macos")]
fn exclusion_for(pb: &NSPasteboard) -> Option<ClipboardExclusionKind> {
    let candidates = NSArray::from_retained_slice(&[
        NSString::from_str(MARKER_CONCEALED),
        NSString::from_str(MARKER_TRANSIENT),
    ]);
    let present = pb.availableTypeFromArray(&candidates)?;
    let ty = present.to_string();
    if ty == MARKER_TRANSIENT {
        Some(ClipboardExclusionKind::Transient)
    } else {
        // `availableTypeFromArray` only returns a type we listed, and the
        // array lists `Concealed` first, so anything that is not the
        // transient marker is the concealed one.
        Some(ClipboardExclusionKind::Concealed)
    }
}

/// Probe `NSPasteboard` for any single representation whose byte length
/// exceeds `max_bytes`, returning the observed length on first hit.
///
/// `NSData::length` is constant-time and avoids the `to_vec()` copy that
/// `ns_data_to_vec` would otherwise perform. `NSString::len` reports exact
/// UTF-8 byte length without materialising a Rust `String`, so text and file
/// URL probes can be compared directly against `max_bytes`.
#[cfg(target_os = "macos")]
fn oversized_payload(max_bytes: usize) -> Option<usize> {
    // Same rationale as `collect_macos_extras`: drain the AppKit
    // autoreleased temporaries on every call so the blocking-pool thread
    // does not retain pasteboard data past return.
    objc2::rc::autoreleasepool(|_pool| {
        // SAFETY: AppKit FFI on the shared pasteboard. All getters return
        // optional retained references and we only read lengths on the
        // returned objects, which has no observable side effects and does
        // not require holding the pasteboard lock beyond the call itself.
        unsafe {
            let pb = NSPasteboard::generalPasteboard();

            if let Some(items) = pb.pasteboardItems()
                && let Some(observed) = oversized_file_urls(&items, max_bytes)
            {
                return Some(observed);
            }
            if let Some(data) = pb.dataForType(NSPasteboardTypePNG)
                && data.length() > max_bytes
            {
                return Some(data.length());
            }
            if let Some(string) = pb.stringForType(NSPasteboardTypeHTML)
                && string.len() > max_bytes
            {
                return Some(string.len());
            }
            if let Some(string) = pb.stringForType(NSPasteboardTypeRTF)
                && string.len() > max_bytes
            {
                return Some(string.len());
            }
            if let Some(string) = pb.stringForType(NSPasteboardTypeString)
                && string.len() > max_bytes
            {
                return Some(string.len());
            }
        }
        None
    })
}

#[cfg(target_os = "macos")]
fn oversized_file_urls(items: &NSArray<NSPasteboardItem>, max_bytes: usize) -> Option<usize> {
    let mut observed_bytes = 0_usize;
    let mut file_url_count = 0_usize;

    for item in items {
        // SAFETY: `NSPasteboardTypeFileURL` is a static AppKit pasteboard type
        // constant with framework lifetime.
        let Some(string) = item.stringForType(unsafe { NSPasteboardTypeFileURL }) else {
            continue;
        };
        file_url_count = file_url_count.saturating_add(1);
        observed_bytes = observed_bytes.saturating_add(string.len());

        if file_url_count > MAX_FILE_URL_ITEMS {
            return Some(observed_bytes.max(limit_exceeded_bytes(max_bytes)));
        }
        if observed_bytes > max_bytes {
            return Some(observed_bytes);
        }
    }

    None
}

#[cfg(target_os = "macos")]
const fn limit_exceeded_bytes(limit: usize) -> usize {
    limit.saturating_add(1)
}

#[cfg(target_os = "macos")]
fn ns_data_to_vec(data: &objc2_foundation::NSData) -> Option<Vec<u8>> {
    if data.is_empty() {
        return None;
    }
    Some(data.to_vec())
}

/// Outcome of inspecting a pasteboard file URL string.
#[cfg(target_os = "macos")]
enum FileUrlResolution {
    /// A file reference URL resolved to this path-based `file://` URL string.
    Resolved(String),
    /// Not a reference URL (or unparseable as an `NSURL`) — decode the raw
    /// pasteboard string directly, preserving prior behaviour for plain path
    /// URLs.
    Raw,
    /// A file reference URL whose target no longer resolves (deleted file,
    /// unmounted volume). There is no real path to show, so the entry is
    /// dropped rather than leaking the `/.file/id=…` handle.
    Drop,
}

/// Resolve a pasteboard file URL string to a path-based `file://` URL.
///
/// Finder copies often publish a *file reference URL*
/// (`file:///.file/id=6571367.5488049`, an inode-based handle) rather than a
/// path URL. The `url` crate can't resolve that form, so [`file_url_to_path`]
/// would surface the literal `/.file/id=…` to the palette even though pasting
/// the entry elsewhere yields the real path. `NSURL::filePathURL` resolves a
/// reference URL to its path-based equivalent; only file reference URLs are
/// routed through it so an unresolvable handle is dropped while plain path
/// URLs keep their existing raw-string decode.
#[cfg(target_os = "macos")]
fn resolve_file_url(raw: &NSString) -> FileUrlResolution {
    let Some(url) = NSURL::URLWithString(raw) else {
        return FileUrlResolution::Raw;
    };
    if !url.isFileReferenceURL() {
        return FileUrlResolution::Raw;
    }
    match url
        .filePathURL()
        .and_then(|resolved| resolved.absoluteString())
    {
        Some(abs) => FileUrlResolution::Resolved(abs.to_string()),
        None => FileUrlResolution::Drop,
    }
}

#[cfg(target_os = "macos")]
fn file_url_to_path(raw: &str) -> Option<String> {
    let parsed = url::Url::parse(raw).ok()?;
    if parsed.scheme() != "file" {
        return None;
    }
    parsed
        .to_file_path()
        .ok()
        .and_then(|path| path.into_os_string().into_string().ok())
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests;
