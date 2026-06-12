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
    clipboard_blocking, clipboard_write_blocking, has_publishable_representation, lock_err,
    platform_err,
};
#[cfg(target_os = "macos")]
use nagori_platform::{DecodeRgbaError, decode_rgba_with_pixel_cap};
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
        .map_err(|err| AppError::Platform(err.to_string()))?
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
        clipboard_blocking("current_snapshot_with_max", move || {
            capture_snapshot_with_max(&clipboard, max_bytes)
        })
        .await
        .map_err(|err| AppError::Platform(err.to_string()))?
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
    const MAX_RETRIES: usize = SNAPSHOT_CAPTURE_MAX_RETRIES;
    for attempt in 1..=MAX_RETRIES {
        match capture_attempt(clipboard, max_bytes)? {
            CaptureAttempt::Settled(snapshot) => return Ok(snapshot),
            CaptureAttempt::Torn(snapshot) => {
                if attempt == MAX_RETRIES {
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
            clipboard
                .lock()
                .map_err(|err| lock_err(&err))?
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
            let _guard = clipboard.lock().map_err(|err| lock_err(&err))?;
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
            // `publish_representations`.
            let _guard = clipboard.lock().map_err(|err| lock_err(&err))?;
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
            // fallback still missing.
            let _guard = clipboard.lock().map_err(|err| lock_err(&err))?;
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
            // shortcuts commonly publish TIFF only; normalize that to PNG
            // before the entry-size gate so the uncompressed pasteboard form
            // does not make ordinary screenshots look oversized. Both
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
                && let Some((mime_type, bytes)) = prepare_tiff_capture(bytes)
            {
                if let Some(limit) = max_file_url_bytes
                    && bytes.len() > limit
                {
                    return Some(bytes.len());
                }
                out.push(ClipboardRepresentation {
                    mime_type,
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
mod tests {
    use super::*;
    use nagori_core::{
        EntryFactory, ImageContent, RepresentationRole, StoredClipboardRepresentation,
    };
    use objc2_foundation::NSArray;

    /// Smallest valid 1x1 transparent PNG; same fixture used by the
    /// `scripts/e2e-macos.sh` capture step.
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xf8,
        0xcf, 0xc0, 0xf0, 0x00, 0x00, 0x03, 0x06, 0x01, 0x80, 0x5a, 0x34, 0x76, 0xf6, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    /// Minimal little-endian 1x1 grayscale TIFF (9 IFD entries, single
    /// white pixel). Generated by hand so the test does not need to depend
    /// on an image-encoding crate.
    const TINY_TIFF: &[u8] = &[
        0x49, 0x49, 0x2a, 0x00, 0x08, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x01, 0x03, 0x00, 0x01,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x01, 0x00, 0x00, 0x00, 0x02, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00,
        0x00, 0x03, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x06, 0x01,
        0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x11, 0x01, 0x04, 0x00, 0x01,
        0x00, 0x00, 0x00, 0x7a, 0x00, 0x00, 0x00, 0x15, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x01, 0x00, 0x00, 0x00, 0x16, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
        0x00, 0x17, 0x01, 0x04, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0xff,
    ];

    fn image_entry(bytes: Vec<u8>, mime: &str) -> ClipboardEntry {
        let byte_count = bytes.len();
        EntryFactory::from_content(
            ClipboardContent::Image(ImageContent {
                width: Some(1),
                height: Some(1),
                byte_count,
                mime_type: Some(mime.to_owned()),
                pending_bytes: Some(bytes),
            }),
            None,
            None,
        )
    }

    fn file_url_items(urls: &[String]) -> objc2::rc::Retained<NSArray<NSPasteboardItem>> {
        let items = urls
            .iter()
            .map(|url| {
                let item = NSPasteboardItem::new();
                let value = NSString::from_str(url);
                // SAFETY: `NSPasteboardTypeFileURL` is a static AppKit
                // pasteboard type constant with framework lifetime.
                assert!(item.setString_forType(&value, unsafe { NSPasteboardTypeFileURL }));
                item
            })
            .collect::<Vec<_>>();
        NSArray::from_retained_slice(&items)
    }

    fn snapshot_bytes(snapshot: &ClipboardSnapshot, mime: &str) -> Option<Vec<u8>> {
        snapshot
            .representations
            .iter()
            .find_map(|rep| match (&rep.data, rep.mime_type.as_str()) {
                (ClipboardData::Bytes(bytes), m) if m == mime => Some(bytes.clone()),
                _ => None,
            })
    }

    #[test]
    fn tiff_capture_is_normalized_to_png() {
        let (mime, bytes) =
            prepare_tiff_capture(TINY_TIFF.to_vec()).expect("tiny tiff passes the pixel cap");

        assert_eq!(mime, "image/png");
        assert!(bytes.starts_with(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]));
    }

    #[test]
    fn prepare_tiff_capture_accepts_small_dimensions() {
        let prepared = prepare_tiff_capture(TINY_TIFF.to_vec());

        let (mime, _) = prepared.expect("tiny tiff is well under the pixel cap");
        assert_eq!(mime, "image/png");
    }

    #[test]
    fn prepare_tiff_capture_rejects_unparseable_tiff() {
        // A TIFF whose IFD declares 65535x65535 (well over the pixel cap)
        // but whose internal strip metadata is inconsistent. The `image`
        // crate's tiff decoder refuses to surface dimensions, so
        // `prepare_tiff_capture` drops the rep instead of letting
        // `decode()` panic or allocate against a corrupt header.
        let mut tiff = TINY_TIFF.to_vec();
        tiff[18] = 0xFF; // ImageWidth low byte
        tiff[19] = 0xFF; // ImageWidth high byte
        tiff[30] = 0xFF; // ImageLength low byte
        tiff[31] = 0xFF; // ImageLength high byte

        assert!(prepare_tiff_capture(tiff).is_none());
    }

    /// Bypass `current_snapshot` and read the raw `NSPasteboardTypeTIFF`
    /// payload — the snapshot reader prefers PNG, so a stale PNG from a
    /// prior step would otherwise satisfy a TIFF round-trip assertion.
    fn read_pasteboard_tiff_bytes() -> Option<Vec<u8>> {
        // SAFETY: AppKit FFI on the shared pasteboard. `NSPasteboardTypeTIFF`
        // is a `'static` extern constant; the returned `Retained<NSData>`
        // owns its bytes independently of any Rust lifetime.
        unsafe {
            let pb = NSPasteboard::generalPasteboard();
            let data = pb.dataForType(NSPasteboardTypeTIFF)?;
            ns_data_to_vec(&data)
        }
    }

    /// Read raw `NSPasteboard` data for the given UTI. JPEG / GIF / WebP
    /// do not have a stable `NSPasteboardType*` constant, so a round-trip
    /// assertion has to materialise an `NSString` for the UTI on the spot.
    fn read_pasteboard_data_for_uti(uti: &str) -> Option<Vec<u8>> {
        let pb = NSPasteboard::generalPasteboard();
        let ty = NSString::from_str(uti);
        let data = pb.dataForType(&ty)?;
        ns_data_to_vec(&data)
    }

    /// Read the file URL string from the first pasteboard item. Used to
    /// verify a single-file `text/uri-list` round-trip; `stringForType`
    /// reports the value from the first item that carries the type, which is
    /// the file item the `writeObjects` batch produced.
    fn read_pasteboard_file_url_string() -> Option<String> {
        // SAFETY: AppKit FFI on the shared pasteboard. `NSPasteboardTypeFileURL`
        // is a `'static` extern constant; the returned `Retained<NSString>`
        // owns its bytes independently of any Rust lifetime.
        unsafe {
            let pb = NSPasteboard::generalPasteboard();
            let s = pb.stringForType(NSPasteboardTypeFileURL)?;
            Some(s.to_string())
        }
    }

    /// Cover PNG / TIFF / text / unsupported-mime / multi-rep cases in one
    /// test so they share a single serialized run against the system
    /// pasteboard. Splitting them into separate `#[tokio::test]`s would let
    /// cargo's thread pool race them on the singleton `NSPasteboard`.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn write_entry_round_trips_image_and_text() {
        let clipboard = match MacosClipboard::new() {
            Ok(clipboard) => clipboard,
            Err(AppError::Platform(message))
                if message.contains("selected clipboard is not supported") =>
            {
                return;
            }
            Err(err) => panic!("init MacosClipboard: {err:?}"),
        };

        let png_entry = image_entry(TINY_PNG.to_vec(), "image/png");
        clipboard
            .write_entry(&png_entry)
            .await
            .expect("write PNG entry");
        let snapshot = clipboard
            .current_snapshot()
            .await
            .expect("snapshot after PNG write");
        let png_back =
            snapshot_bytes(&snapshot, "image/png").expect("image/png missing from snapshot");
        assert_eq!(
            png_back, TINY_PNG,
            "PNG bytes must round-trip through NSPasteboardTypePNG verbatim"
        );

        let tiff_entry = image_entry(TINY_TIFF.to_vec(), "image/tiff");
        clipboard
            .write_entry(&tiff_entry)
            .await
            .expect("write TIFF entry");
        // Read the TIFF type directly off the pasteboard. Going through
        // `current_snapshot` here would let stale PNG bytes from the prior
        // step satisfy the assertion, since `collect_macos_extras` prefers
        // PNG when both types are present.
        let tiff_back = tokio::task::spawn_blocking(read_pasteboard_tiff_bytes)
            .await
            .expect("join blocking read");
        assert_eq!(
            tiff_back.as_deref(),
            Some(TINY_TIFF),
            "TIFF bytes must round-trip through NSPasteboardTypeTIFF verbatim"
        );

        let text_entry = EntryFactory::from_text("write_entry text fallback round-trip");
        clipboard
            .write_entry(&text_entry)
            .await
            .expect("write text entry");
        let snapshot = clipboard
            .current_snapshot()
            .await
            .expect("snapshot after text write");
        let text_back = snapshot
            .representations
            .iter()
            .find_map(|rep| match &rep.data {
                ClipboardData::Text(t) if rep.mime_type == "text/plain" => Some(t.clone()),
                _ => None,
            })
            .expect("text/plain missing from snapshot");
        assert_eq!(text_back, "write_entry text fallback round-trip");

        // JPEG / GIF / WebP go through dynamic UTI strings rather
        // than the static `NSPasteboardType*` constants — round-trip each
        // through its UTI to confirm the publisher's match arm fires and
        // AppKit accepts the bytes verbatim.
        for (mime, uti, bytes) in [
            (
                "image/jpeg",
                UTI_JPEG,
                &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x01, 0x02][..],
            ),
            ("image/gif", UTI_GIF, &[0xCA, 0xFE, 0xBA, 0xBE][..]),
            (
                "image/webp",
                UTI_WEBP,
                &[0x52, 0x49, 0x46, 0x46, 0x77, 0x65, 0x62, 0x70][..],
            ),
        ] {
            let entry = image_entry(bytes.to_vec(), mime);
            clipboard
                .write_entry(&entry)
                .await
                .unwrap_or_else(|err| panic!("write {mime} entry: {err:?}"));
            let read_back = tokio::task::spawn_blocking(move || read_pasteboard_data_for_uti(uti))
                .await
                .expect("join blocking read");
            assert_eq!(
                read_back.as_deref(),
                Some(bytes),
                "{mime} bytes must round-trip through {uti} verbatim"
            );
        }

        let truly_unsupported = image_entry(vec![0x00, 0x01, 0x02], "image/heic");
        let err = clipboard
            .write_entry(&truly_unsupported)
            .await
            .expect_err("write_entry must reject genuinely unsupported image mime types");
        assert!(
            matches!(err, AppError::Unsupported(_)),
            "expected AppError::Unsupported for image/heic, got {err:?}"
        );

        // Preserve copy-back: a rich-text entry should land HTML + plain
        // fallback + RTF on the pasteboard in a single atomic batch so a
        // downstream paste target can pick the richest representation the
        // source originally offered.
        let host_entry =
            EntryFactory::from_text("write_representations rich-text round-trip plain body");
        let reps = vec![
            StoredClipboardRepresentation {
                role: RepresentationRole::Primary,
                mime_type: "text/html".to_owned(),
                ordinal: 0,
                data: RepresentationDataRef::InlineText(
                    "<p>write_representations rich-text round-trip <strong>html</strong></p>"
                        .to_owned(),
                ),
            },
            StoredClipboardRepresentation {
                role: RepresentationRole::PlainFallback,
                mime_type: "text/plain".to_owned(),
                ordinal: 1,
                data: RepresentationDataRef::InlineText(
                    "write_representations rich-text round-trip plain body".to_owned(),
                ),
            },
            StoredClipboardRepresentation {
                role: RepresentationRole::Alternative,
                mime_type: "application/rtf".to_owned(),
                ordinal: 2,
                data: RepresentationDataRef::InlineText("{\\rtf1\\ansi rich body}".to_owned()),
            },
        ];
        clipboard
            .write_representations(&host_entry, &reps)
            .await
            .expect("write_representations multi-rep batch");
        let snapshot = clipboard
            .current_snapshot()
            .await
            .expect("snapshot after multi-rep write");
        let html_back = snapshot
            .representations
            .iter()
            .find_map(|rep| match &rep.data {
                ClipboardData::Text(t) if rep.mime_type == "text/html" => Some(t.clone()),
                _ => None,
            })
            .expect("text/html missing from snapshot after multi-rep write");
        assert!(
            html_back.contains("<strong>html</strong>"),
            "expected HTML rep to survive the multi-rep write, got {html_back:?}"
        );
        let plain_back = snapshot
            .representations
            .iter()
            .find_map(|rep| match &rep.data {
                ClipboardData::Text(t) if rep.mime_type == "text/plain" => Some(t.clone()),
                _ => None,
            })
            .expect("text/plain missing from snapshot after multi-rep write");
        assert_eq!(
            plain_back, "write_representations rich-text round-trip plain body",
            "plain fallback must be published alongside the HTML primary",
        );

        // Empty representation set must fall back to write_entry semantics so
        // a caller that hands in an unhydrated list still publishes the
        // primary content rather than silently leaving the pasteboard empty.
        let fallback_entry = EntryFactory::from_text(
            "write_representations empty-fallback delegates to write_entry",
        );
        clipboard
            .write_representations(&fallback_entry, &[])
            .await
            .expect("empty representations must fall back to write_entry");
        let snapshot = clipboard
            .current_snapshot()
            .await
            .expect("snapshot after empty-fallback write");
        let fallback_back = snapshot
            .representations
            .iter()
            .find_map(|rep| match &rep.data {
                ClipboardData::Text(t) if rep.mime_type == "text/plain" => Some(t.clone()),
                _ => None,
            })
            .expect("text/plain missing after empty-fallback write");
        assert_eq!(
            fallback_back, "write_representations empty-fallback delegates to write_entry",
            "empty rep list should publish entry plain text via write_entry",
        );

        // Round-trip a single-file `text/uri-list` rep. The publisher writes
        // one `NSPasteboardItem` per file URL via `writeObjects`, so a
        // single-file list produces a single file item any Finder / TextEdit
        // paste target reads the same way it would a Finder copy.
        let file_url_entry = EntryFactory::from_text("/tmp/nagori-uri-list-roundtrip");
        let file_url_reps = vec![StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "text/uri-list".to_owned(),
            ordinal: 0,
            data: RepresentationDataRef::FilePaths(vec![
                "/tmp/nagori-uri-list-roundtrip".to_owned(),
            ]),
        }];
        clipboard
            .write_representations(&file_url_entry, &file_url_reps)
            .await
            .expect("write_representations file-URL rep");
        let url_back = tokio::task::spawn_blocking(read_pasteboard_file_url_string)
            .await
            .expect("join blocking read");
        assert_eq!(
            url_back.as_deref(),
            Some("file:///tmp/nagori-uri-list-roundtrip"),
            "text/uri-list rep must publish a file:// URL on NSPasteboardTypeFileURL"
        );

        // Multi-file `text/uri-list` round-trip through the multi-rep path.
        // The old per-rep `setString_forType` loop collapsed every path onto
        // the implicit item's single file-URL slot; the `NSPasteboardItem`
        // batch must instead keep every path so Finder pastes all of them.
        let multi_paths = vec![
            "/tmp/nagori-multi-one".to_owned(),
            "/tmp/nagori-multi-two".to_owned(),
            "/tmp/nagori-multi-three".to_owned(),
        ];
        let multi_entry = EntryFactory::from_text("/tmp/nagori-multi-one");
        let multi_reps = vec![StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "text/uri-list".to_owned(),
            ordinal: 0,
            data: RepresentationDataRef::FilePaths(multi_paths.clone()),
        }];
        clipboard
            .write_representations(&multi_entry, &multi_reps)
            .await
            .expect("write_representations multi-file uri-list");
        let multi_back = clipboard
            .current_snapshot()
            .await
            .expect("snapshot after multi-file uri-list write");
        let captured = multi_back
            .representations
            .iter()
            .find_map(|rep| match &rep.data {
                ClipboardData::FilePaths(paths) if rep.mime_type == "text/uri-list" => {
                    Some(paths.clone())
                }
                _ => None,
            })
            .expect("text/uri-list missing after multi-file write");
        assert_eq!(
            captured, multi_paths,
            "every file path must survive the multi-file copy-back, not collapse to the last URL",
        );

        // The primary-only `write_entry` FileList branch must publish files
        // too — before the fix it fell through to `plain_text()` and pasted
        // the paths as text, which never lands in Finder as files.
        let file_list_entry = EntryFactory::from_content(
            ClipboardContent::FileList(nagori_core::FileListContent {
                paths: multi_paths.clone(),
                display_text: multi_paths.join("\n"),
            }),
            None,
            None,
        );
        clipboard
            .write_entry(&file_list_entry)
            .await
            .expect("write_entry FileList branch");
        let entry_back = clipboard
            .current_snapshot()
            .await
            .expect("snapshot after write_entry FileList");
        let entry_paths = entry_back
            .representations
            .iter()
            .find_map(|rep| match &rep.data {
                ClipboardData::FilePaths(paths) if rep.mime_type == "text/uri-list" => {
                    Some(paths.clone())
                }
                _ => None,
            })
            .expect("text/uri-list missing after write_entry FileList");
        assert_eq!(
            entry_paths, multi_paths,
            "write_entry must republish a FileList as file URLs, not plain text",
        );

        // An empty FileList must be refused rather than blanking the clipboard.
        let empty_file_list = EntryFactory::from_content(
            ClipboardContent::FileList(nagori_core::FileListContent {
                paths: vec![],
                display_text: String::new(),
            }),
            None,
            None,
        );
        let empty_err = clipboard
            .write_entry(&empty_file_list)
            .await
            .expect_err("empty file-list copy-back must be refused");
        assert!(
            matches!(empty_err, AppError::Unsupported(_)),
            "expected AppError::Unsupported for an empty file list, got {empty_err:?}"
        );

        // A rep set whose MIMEs are all outside the NSPasteboard publisher's
        // table (a MIME we genuinely cannot publish — e.g. `application/pdf`)
        // must fall back to write_entry *before* `clearContents()` runs —
        // otherwise an all-unsupported set would leave the user's clipboard
        // empty and the copy command would surface a platform error.
        let only_unsupported_entry = EntryFactory::from_text(
            "write_representations falls back to write_entry when no rep is publishable",
        );
        let unsupported_reps = vec![StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "application/pdf".to_owned(),
            ordinal: 0,
            data: RepresentationDataRef::DatabaseBlob(vec![0x25, 0x50, 0x44, 0x46]),
        }];
        clipboard
            .write_representations(&only_unsupported_entry, &unsupported_reps)
            .await
            .expect("all-skipped rep set must fall back to write_entry, not error");
        let snapshot = clipboard
            .current_snapshot()
            .await
            .expect("snapshot after all-skipped fallback");
        let fallback_back = snapshot
            .representations
            .iter()
            .find_map(|rep| match &rep.data {
                ClipboardData::Text(t) if rep.mime_type == "text/plain" => Some(t.clone()),
                _ => None,
            })
            .expect("text/plain missing after all-skipped fallback");
        assert_eq!(
            fallback_back,
            "write_representations falls back to write_entry when no rep is publishable",
            "all-skipped rep set should publish entry plain text via write_entry",
        );
    }

    /// A Finder copy usually lands a *file reference URL*
    /// (`file:///.file/id=…`) on the pasteboard rather than a path URL.
    /// `collect_file_url_paths` must resolve it to the real filesystem path
    /// instead of surfacing the literal `/.file/id=…` handle.
    #[test]
    fn file_reference_url_is_resolved_to_real_path() {
        // A reference URL only resolves while its target exists, so back it
        // with a real file. A unique name keeps parallel test binaries apart.
        let path = std::env::temp_dir().join(format!("nagori-file-ref-{}", std::process::id()));
        std::fs::write(&path, b"nagori").expect("write temp file");
        let real = std::fs::canonicalize(&path).expect("canonicalize temp file");

        // Mint the reference URL the way Finder does and grab its string form.
        let path_str = path.to_str().expect("temp path is valid UTF-8");
        let path_url = NSURL::fileURLWithPath(&NSString::from_str(path_str));
        let Some(reference) = path_url.fileReferenceURL() else {
            // Some volumes (e.g. network mounts on CI) can't vend reference
            // URLs; there's nothing to assert in that environment.
            std::fs::remove_file(&path).ok();
            return;
        };
        let reference_string = reference
            .absoluteString()
            .expect("reference URL has an absolute string")
            .to_string();
        assert!(
            reference_string.contains("/.file/id="),
            "expected a file reference URL, got {reference_string}"
        );

        let items = file_url_items(&[reference_string]);
        let captured = collect_file_url_paths(&items, Some(64 * 1024));
        let FileUrlPaths::Captured(paths) = captured else {
            std::fs::remove_file(&path).ok();
            panic!("a single resolvable file reference URL must be captured");
        };
        assert_eq!(paths.len(), 1, "expected exactly one resolved path");
        let resolved = std::fs::canonicalize(&paths[0]).expect("resolved path exists");
        std::fs::remove_file(&path).ok();

        assert!(
            !paths[0].contains("/.file/id="),
            "file reference URL leaked unresolved: {}",
            paths[0]
        );
        assert_eq!(resolved, real);
    }

    /// A file reference URL whose target can't be resolved (here a bogus id)
    /// must be dropped rather than leaking the `/.file/id=…` handle that the
    /// `url` crate would otherwise decode verbatim.
    #[test]
    fn unresolvable_file_reference_url_is_dropped() {
        let bogus = "file:///.file/id=999999999.999999999".to_owned();
        let items = file_url_items(&[bogus]);

        let FileUrlPaths::Captured(paths) = collect_file_url_paths(&items, Some(64 * 1024)) else {
            panic!("a single file URL must be captured under the limits");
        };
        assert!(
            paths.is_empty(),
            "an unresolvable reference URL should be dropped, got {paths:?}"
        );
    }

    #[test]
    fn file_url_paths_are_captured_under_limits() {
        let urls = vec!["file:///tmp/nagori%20one".to_owned()];
        let items = file_url_items(&urls);

        let FileUrlPaths::Captured(paths) = collect_file_url_paths(&items, Some(1024)) else {
            panic!("file URL under the byte and count limits must be captured");
        };

        assert_eq!(paths, vec!["/tmp/nagori one"]);
        assert_eq!(oversized_file_urls(&items, 1024), None);
    }

    #[test]
    fn file_url_probe_rejects_total_utf8_bytes_before_path_allocation() {
        let urls = vec![
            "file:///tmp/nagori-alpha".to_owned(),
            "file:///tmp/nagori-beta".to_owned(),
        ];
        let items = file_url_items(&urls);
        let limit = urls[0].len();

        let Some(observed) = oversized_file_urls(&items, limit) else {
            panic!("aggregate file URL bytes above the limit must be oversized");
        };
        assert!(observed > limit);

        let FileUrlPaths::Oversized(collected_observed) =
            collect_file_url_paths(&items, Some(limit))
        else {
            panic!("bounded file URL collection must stop before building a full path list");
        };
        assert_eq!(collected_observed, observed);
    }

    #[test]
    fn file_url_probe_rejects_too_many_items() {
        let urls = (0..=MAX_FILE_URL_ITEMS)
            .map(|index| format!("file:///tmp/nagori-{index}"))
            .collect::<Vec<_>>();
        let items = file_url_items(&urls);
        let limit = 1024 * 1024;

        let Some(observed) = oversized_file_urls(&items, limit) else {
            panic!("file URL count above the item limit must be oversized");
        };
        assert!(observed > limit);

        let FileUrlPaths::Oversized(collected_observed) =
            collect_file_url_paths(&items, Some(limit))
        else {
            panic!("bounded file URL collection must reject excessive item counts");
        };
        assert_eq!(collected_observed, observed);
    }

    #[test]
    fn unbounded_file_url_collection_still_caps_item_count() {
        // The `current_snapshot` path passes `max_bytes = None`. Without an
        // unconditional count cap a pasteboard advertising millions of file
        // URLs would grow `paths` without bound, so the cap must fire even
        // when no byte budget is supplied.
        let urls = (0..=MAX_FILE_URL_ITEMS)
            .map(|index| format!("file:///tmp/nagori-{index}"))
            .collect::<Vec<_>>();
        let items = file_url_items(&urls);

        let FileUrlPaths::Oversized(observed) = collect_file_url_paths(&items, None) else {
            panic!("unbounded file URL collection must still reject excessive item counts");
        };
        assert!(observed > 0);

        // A list at or below the cap is still captured on the unbounded path.
        let few = (0..8)
            .map(|index| format!("file:///tmp/nagori-{index}"))
            .collect::<Vec<_>>();
        let few_items = file_url_items(&few);
        let FileUrlPaths::Captured(paths) = collect_file_url_paths(&few_items, None) else {
            panic!("a small file URL list must be captured on the unbounded path");
        };
        assert_eq!(paths.len(), few.len());
    }
}
