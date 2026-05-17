use std::sync::{Arc, Mutex};

use arboard::Clipboard;
use async_trait::async_trait;
use nagori_core::{
    AppError, ClipboardContent, ClipboardData, ClipboardEntry, ClipboardRepresentation,
    ClipboardSequence, ClipboardSnapshot, RepresentationDataRef, Result,
    StoredClipboardRepresentation,
};
use nagori_platform::{CapturedSnapshot, ClipboardReader, ClipboardWriter};
#[cfg(target_os = "macos")]
use objc2_app_kit::{
    NSPasteboard, NSPasteboardItem, NSPasteboardTypeFileURL, NSPasteboardTypeHTML,
    NSPasteboardTypePNG, NSPasteboardTypeRTF, NSPasteboardTypeString, NSPasteboardTypeTIFF,
};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSArray, NSData, NSString};
use time::OffsetDateTime;

#[cfg(target_os = "macos")]
const MAX_FILE_URL_ITEMS: usize = 4096;

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
        tokio::task::spawn_blocking(move || -> Result<ClipboardSnapshot> {
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
        tokio::task::spawn_blocking(pasteboard_sequence)
            .await
            .map_err(|err| AppError::Platform(err.to_string()))
    }

    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    async fn current_snapshot_with_max(&self, max_bytes: usize) -> Result<CapturedSnapshot> {
        // Same locking discipline as `current_snapshot` — hold the arboard
        // mutex across both the AppKit size probe and the per-rep load so a
        // concurrent writer cannot race a torn snapshot in between.
        let clipboard = self.clipboard.clone();
        tokio::task::spawn_blocking(move || -> Result<CapturedSnapshot> {
            let mut guard = clipboard.lock().map_err(|err| lock_err(&err))?;

            // Phase 1: peek byte sizes without materialising payloads. On
            // macOS, NSData backs each `dataForType` result with bytes
            // already paged into our address space, but skipping `to_vec()`
            // still avoids the second copy into a Rust `Vec<u8>` and lets
            // NSData drop on scope exit, freeing both copies promptly.
            // NSString::len() reports UTF-8 bytes without materialising a
            // Rust String. Phase 1 is still only an admission pre-filter:
            // it catches oversized single reps and file URL aggregates
            // before we allocate Rust payload buffers, while the capture
            // loop's post-load check remains authoritative for the final
            // ClipboardEntry payload.
            #[cfg(target_os = "macos")]
            if let Some(observed) = oversized_payload(max_bytes) {
                drop(guard);
                return Ok(CapturedSnapshot::Oversized {
                    sequence: pasteboard_sequence(),
                    observed_bytes: observed,
                    limit: max_bytes,
                });
            }

            // Phase 2: load the snapshot. Phase 1 only rejected the
            // obvious oversize cases; reps that pass it can still grow
            // past `max_bytes` once decoded to UTF-8, and the aggregate
            // of multiple reps is not bounded here at all. The capture
            // loop's post-load `payload_bytes > max_entry_size_bytes`
            // check is the authoritative limit — Phase 1 just spares
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
                drop(guard);
                return Ok(CapturedSnapshot::Oversized {
                    sequence: pasteboard_sequence(),
                    observed_bytes: observed,
                    limit: max_bytes,
                });
            }

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
            Ok(CapturedSnapshot::Captured(snapshot))
        })
        .await
        .map_err(|err| AppError::Platform(err.to_string()))?
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
        tokio::task::spawn_blocking(move || -> Result<()> {
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
}

impl MacosClipboard {
    #[cfg(target_os = "macos")]
    async fn write_image_bytes(&self, bytes: Vec<u8>, mime: &str) -> Result<()> {
        let mime_owned = mime.to_owned();
        let clipboard = self.clipboard.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            // Take the same arboard mutex `current_snapshot` and the text
            // path use so a concurrent reader/writer cannot race the
            // clearContents+setData pair below on the shared NSPasteboard.
            let _guard = clipboard.lock().map_err(|err| lock_err(&err))?;
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
                let pb = NSPasteboard::generalPasteboard();
                pb.clearContents();
                let data = NSData::with_bytes(&bytes);
                let accepted = match mime_owned.as_str() {
                    "image/png" => pb.setData_forType(Some(&data), NSPasteboardTypePNG),
                    "image/tiff" => pb.setData_forType(Some(&data), NSPasteboardTypeTIFF),
                    "image/jpeg" => {
                        let ty = NSString::from_str(UTI_JPEG);
                        pb.setData_forType(Some(&data), &ty)
                    }
                    "image/gif" => {
                        let ty = NSString::from_str(UTI_GIF);
                        pb.setData_forType(Some(&data), &ty)
                    }
                    "image/webp" => {
                        let ty = NSString::from_str(UTI_WEBP);
                        pb.setData_forType(Some(&data), &ty)
                    }
                    other => {
                        return Err(AppError::Unsupported(format!(
                            "unsupported image clipboard mime type: {other}"
                        )));
                    }
                };
                if !accepted {
                    return Err(AppError::Platform(
                        "NSPasteboard::setData failed for image type".to_owned(),
                    ));
                }
                Ok(())
            }
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
    async fn publish_representations(
        &self,
        representations: Vec<StoredClipboardRepresentation>,
    ) -> Result<()> {
        let clipboard = self.clipboard.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            // Hold the arboard mutex across the whole clearContents + setData
            // batch so a concurrent reader cannot observe a partial state with
            // the primary published but the plain fallback still missing.
            let _guard = clipboard.lock().map_err(|err| lock_err(&err))?;
            objc2::rc::autoreleasepool(|_pool| -> Result<()> {
                // SAFETY: AppKit FFI on the shared `NSPasteboard`. The pasteboard
                // type constants are `'static` extern symbols owned by AppKit, and
                // every `NSString`/`NSData` we hand off is freshly retained on the
                // ObjC heap so it does not depend on any Rust lifetime once the
                // call returns. `clearContents` plus the per-type set calls happen
                // under the arboard mutex above, so no concurrent writer can race
                // a torn batch between them.
                unsafe {
                    let pb = NSPasteboard::generalPasteboard();
                    pb.clearContents();
                    let mut published = 0_usize;
                    for rep in &representations {
                        match publish_one_representation(&pb, rep) {
                            PublishOutcome::Published => {
                                published = published.saturating_add(1);
                            }
                            PublishOutcome::Skipped => {}
                            PublishOutcome::Failed => {
                                // AppKit accepted the mapping but
                                // `setString` / `setData` returned NO — keep
                                // going so the rest of the rep set still
                                // lands on the pasteboard, but surface the
                                // failure at warn so a primary HTML / image
                                // drop is visible in logs instead of being
                                // hidden behind the surviving plain
                                // fallback.
                                tracing::warn!(
                                    mime = %rep.mime_type,
                                    role = ?rep.role,
                                    ordinal = rep.ordinal,
                                    "NSPasteboard rejected setString/setData for stored representation",
                                );
                            }
                        }
                    }
                    if published == 0 {
                        // Pre-scan in `write_representations` rules this out
                        // in normal use; the only way to reach it now is if
                        // every mapped rep above hit `Failed`. The pasteboard
                        // is already empty — surface the platform error so
                        // the daemon's `copy_entry_with_format` propagates
                        // it instead of silently leaving the user with no
                        // clipboard contents.
                        return Err(AppError::Platform(
                            "NSPasteboard rejected every mapped representation"
                                .to_owned(),
                        ));
                    }
                    Ok(())
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
            "multi-representation clipboard writes are macOS-only".to_owned(),
        ))
    }
}

/// Result of attempting to publish one stored representation.
///
/// `Skipped` and `Failed` are kept distinct so the caller can warn about a
/// MIME we promised to publish but `AppKit` rejected, without spamming a warn
/// every time a `text/uri-list` rep flows through (we know we can't publish
/// those — that is a `Skipped`, not a bug).
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

/// True when at least one rep has a known `NSPasteboardType` mapping.
///
/// Used as a pre-scan before `clearContents()` so an entry whose stored
/// reps are *all* outside the macOS publisher's MIME table (file URLs,
/// `image/jpeg`, etc.) falls back through `write_entry` instead of clearing
/// the pasteboard and erroring after the fact. The body only inspects MIME
/// strings and `RepresentationDataRef` variants from `nagori-core`, so it
/// stays `cfg`-free even though its only caller is the macOS impl — the
/// crate is a workspace member built on every host and the helper has to
/// resolve on non-mac targets too.
fn has_publishable_representation(reps: &[StoredClipboardRepresentation]) -> bool {
    reps.iter()
        .any(|rep| match (rep.mime_type.as_str(), &rep.data) {
            (
                "text/plain" | "text/html" | "application/rtf",
                RepresentationDataRef::InlineText(_),
            )
            | (
                "image/png" | "image/tiff" | "image/jpeg" | "image/gif" | "image/webp",
                RepresentationDataRef::DatabaseBlob(_),
            ) => true,
            ("text/uri-list", RepresentationDataRef::FilePaths(paths)) => !paths.is_empty(),
            _ => false,
        })
}

/// Publish a single stored representation onto the shared `NSPasteboard`.
///
/// The MIME → `NSPasteboardType` table covers plain text, HTML, RTF, PNG,
/// TIFF, JPEG, GIF, WebP, and `text/uri-list` file lists. Image MIMEs that
/// lack a static `NSPasteboardType*` constant (JPEG/GIF/WebP) are published
/// against their canonical Uniform Type Identifier strings — `AppKit` copies
/// the type name, so the dynamic `NSString` only has to outlive
/// `setData_forType`. File paths fan out per-item via `setString_forType`
/// on `NSPasteboardTypeFileURL`; multi-file lists currently keep only the
/// last URL on the implicit pasteboard item, which is the Phase 4
/// limitation documented in ARCHITECTURE.md and addressed once
/// `NSPasteboardItem` batches replace the per-rep `setString` loop.
#[cfg(target_os = "macos")]
unsafe fn publish_one_representation(
    pb: &NSPasteboard,
    rep: &StoredClipboardRepresentation,
) -> PublishOutcome {
    let accepted = match (rep.mime_type.as_str(), &rep.data) {
        ("text/plain", RepresentationDataRef::InlineText(text)) => {
            let value = NSString::from_str(text);
            // SAFETY: `pb` is the shared general pasteboard already cleared by
            // the caller, `value` is a freshly retained NSString that AppKit
            // copies, and `NSPasteboardTypeString` is a static framework
            // constant.
            unsafe { pb.setString_forType(&value, NSPasteboardTypeString) }
        }
        ("text/html", RepresentationDataRef::InlineText(text)) => {
            let value = NSString::from_str(text);
            unsafe { pb.setString_forType(&value, NSPasteboardTypeHTML) }
        }
        ("application/rtf", RepresentationDataRef::InlineText(text)) => {
            let value = NSString::from_str(text);
            unsafe { pb.setString_forType(&value, NSPasteboardTypeRTF) }
        }
        ("image/png", RepresentationDataRef::DatabaseBlob(bytes)) => {
            let data = NSData::with_bytes(bytes);
            unsafe { pb.setData_forType(Some(&data), NSPasteboardTypePNG) }
        }
        ("image/tiff", RepresentationDataRef::DatabaseBlob(bytes)) => {
            let data = NSData::with_bytes(bytes);
            unsafe { pb.setData_forType(Some(&data), NSPasteboardTypeTIFF) }
        }
        ("image/jpeg", RepresentationDataRef::DatabaseBlob(bytes)) => {
            // `NSString::from_str` returns a freshly retained NSString
            // that AppKit copies on `setData_forType`. No static-borrow
            // requirements, hence no inner `unsafe` block.
            let data = NSData::with_bytes(bytes);
            let ty = NSString::from_str(UTI_JPEG);
            pb.setData_forType(Some(&data), &ty)
        }
        ("image/gif", RepresentationDataRef::DatabaseBlob(bytes)) => {
            let data = NSData::with_bytes(bytes);
            let ty = NSString::from_str(UTI_GIF);
            pb.setData_forType(Some(&data), &ty)
        }
        ("image/webp", RepresentationDataRef::DatabaseBlob(bytes)) => {
            let data = NSData::with_bytes(bytes);
            let ty = NSString::from_str(UTI_WEBP);
            pb.setData_forType(Some(&data), &ty)
        }
        ("text/uri-list", RepresentationDataRef::FilePaths(paths)) => {
            // The Phase 4 "simple" path: write each file URL via
            // `setString_forType(NSPasteboardTypeFileURL)`. AppKit holds
            // one value per type on the implicit pasteboard item, so a
            // multi-file list collapses to its last URL — Finder / TextEdit
            // still accept a single-file paste, which is the common case
            // and a strict improvement over the previous "skip every
            // file-list rep" behaviour. Switching to `NSPasteboardItem`
            // batches for true multi-file paste is tracked for a later
            // phase.
            let mut any_accepted = false;
            for path in paths {
                let Some(url) = path_to_file_url(path) else {
                    continue;
                };
                let value = NSString::from_str(&url);
                // SAFETY: `NSPasteboardTypeFileURL` is a `'static` AppKit
                // constant, and `value` is a freshly retained NSString
                // copied by AppKit before the call returns.
                if unsafe { pb.setString_forType(&value, NSPasteboardTypeFileURL) } {
                    any_accepted = true;
                }
            }
            any_accepted
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
                let paths = match collect_file_url_paths(&items, max_file_url_bytes) {
                    FileUrlPaths::Captured(paths) => paths,
                    FileUrlPaths::Oversized(observed) => return Some(observed),
                };
                if !paths.is_empty() {
                    out.push(ClipboardRepresentation {
                        mime_type: "text/uri-list".to_owned(),
                        data: ClipboardData::FilePaths(paths),
                    });
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

            // Prefer PNG when both PNG and TIFF are present — the bytes are
            // smaller and every webview can render them directly. We still fall
            // back to TIFF so screenshots from older macOS apps that only push
            // TIFF make it into the history.
            if let Some(data) = pb.dataForType(NSPasteboardTypePNG)
                && let Some(bytes) = ns_data_to_vec(&data)
            {
                out.push(ClipboardRepresentation {
                    mime_type: "image/png".to_owned(),
                    data: ClipboardData::Bytes(bytes),
                });
            } else if let Some(data) = pb.dataForType(NSPasteboardTypeTIFF)
                && let Some(bytes) = ns_data_to_vec(&data)
            {
                out.push(ClipboardRepresentation {
                    mime_type: "image/tiff".to_owned(),
                    data: ClipboardData::Bytes(bytes),
                });
            }
        }
        None
    })
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

        if let Some(limit) = max_bytes {
            if file_url_count > MAX_FILE_URL_ITEMS {
                return FileUrlPaths::Oversized(observed_bytes.max(limit_exceeded_bytes(limit)));
            }
            if observed_bytes > limit {
                return FileUrlPaths::Oversized(observed_bytes);
            }
        }

        let raw = string.to_string();
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
            if let Some(data) = pb.dataForType(NSPasteboardTypeTIFF)
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

fn platform_err(err: &arboard::Error) -> AppError {
    AppError::Platform(err.to_string())
}

fn lock_err<T>(err: &std::sync::PoisonError<T>) -> AppError {
    AppError::Platform(err.to_string())
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

    /// Read the first file URL string parked on the implicit pasteboard
    /// item. Used to verify `text/uri-list` round-trips without going
    /// through `current_snapshot`, which collects file URLs across every
    /// pasteboard item — the per-rep `setString_forType` publish path only
    /// produces a single implicit item.
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

        // Phase 4: JPEG / GIF / WebP go through dynamic UTI strings rather
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

        // Round-trip a single-file `text/uri-list` rep. The publisher
        // writes each path via `setString_forType(NSPasteboardTypeFileURL)`,
        // so a single-file list lands on the implicit pasteboard item and
        // any Finder / TextEdit paste target sees the same URL it would
        // get from a Finder copy.
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
}
