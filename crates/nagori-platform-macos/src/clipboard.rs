use std::sync::{Arc, Mutex};

use arboard::Clipboard;
use async_trait::async_trait;
use nagori_core::{
    AppError, ClipboardContent, ClipboardData, ClipboardEntry, ClipboardRepresentation,
    ClipboardSequence, ClipboardSnapshot, Result,
};
use nagori_platform::{CapturedSnapshot, ClipboardReader, ClipboardWriter};
#[cfg(target_os = "macos")]
use objc2_app_kit::{
    NSPasteboard, NSPasteboardType, NSPasteboardTypeFileURL, NSPasteboardTypeHTML,
    NSPasteboardTypePNG, NSPasteboardTypeRTF, NSPasteboardTypeTIFF,
};
#[cfg(target_os = "macos")]
use objc2_foundation::NSData;
use time::OffsetDateTime;

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
            collect_macos_extras(&mut representations);

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
            // NSString's `length()` is in UTF-16 code units; UTF-8 byte
            // length is always >= UTF-16 unit count, so `length() >
            // max_bytes` is a sound (one-sided) reject gate — it never
            // false-rejects a string that would have fit. The converse
            // does *not* hold: a string with `length() <= max_bytes` can
            // still exceed `max_bytes` after UTF-8 encoding (e.g. CJK at
            // ~3 bytes/codepoint vs. 2 bytes/UTF-16 unit). Phase 1 is
            // therefore a cheap pre-filter for the obvious outliers, not
            // a precise admission check.
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
            collect_macos_extras(&mut representations);

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
                let pasteboard_type: &NSPasteboardType = match mime_owned.as_str() {
                    "image/png" => NSPasteboardTypePNG,
                    "image/tiff" => NSPasteboardTypeTIFF,
                    other => {
                        return Err(AppError::Unsupported(format!(
                            "unsupported image clipboard mime type: {other}"
                        )));
                    }
                };
                let pb = NSPasteboard::generalPasteboard();
                pb.clearContents();
                let data = NSData::with_bytes(&bytes);
                if !pb.setData_forType(Some(&data), pasteboard_type) {
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
fn pasteboard_sequence() -> ClipboardSequence {
    ClipboardSequence::unsupported()
}

#[cfg(target_os = "macos")]
fn collect_macos_extras(out: &mut Vec<ClipboardRepresentation>) {
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
                let mut paths = Vec::new();
                for item in &items {
                    if let Some(string) = item.stringForType(NSPasteboardTypeFileURL) {
                        let raw = string.to_string();
                        if let Some(path) = file_url_to_path(&raw) {
                            paths.push(path);
                        }
                    }
                }
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
    });
}

/// Probe `NSPasteboard` for any single representation whose byte length
/// exceeds `max_bytes`, returning the observed length on first hit.
///
/// `NSData::length` is constant-time and avoids the `to_vec()` copy that
/// `ns_data_to_vec` would otherwise perform. `NSString::length` returns
/// UTF-16 code units; UTF-8 byte length is always >= UTF-16 unit count
/// (every non-empty UTF-16 unit maps to >= 1 UTF-8 byte), so comparing
/// `length() > max_bytes` cannot reject a string that would actually fit.
#[cfg(target_os = "macos")]
fn oversized_payload(max_bytes: usize) -> Option<usize> {
    // Same rationale as `collect_macos_extras`: drain the AppKit
    // autoreleased temporaries on every call so the blocking-pool thread
    // does not retain pasteboard data past return.
    objc2::rc::autoreleasepool(|_pool| {
        // SAFETY: AppKit FFI on the shared pasteboard. All getters return
        // optional retained references and we only read `.length()` on the
        // returned objects, which has no observable side effects and does not
        // require holding the pasteboard lock beyond the call itself.
        unsafe {
            let pb = NSPasteboard::generalPasteboard();

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
                && string.length() > max_bytes
            {
                return Some(string.length());
            }
            if let Some(string) = pb.stringForType(NSPasteboardTypeRTF)
                && string.length() > max_bytes
            {
                return Some(string.length());
            }
            if let Some(string) = pb.stringForType(objc2_app_kit::NSPasteboardTypeString)
                && string.length() > max_bytes
            {
                return Some(string.length());
            }
        }
        None
    })
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
    use nagori_core::{EntryFactory, ImageContent, PayloadRef};

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
                payload_ref: PayloadRef::DatabaseBlob(String::new()),
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

    /// Cover PNG / TIFF / text / unsupported-mime cases in one test so
    /// they share a single serialized run against the system pasteboard.
    /// Splitting them into separate `#[tokio::test]`s would let cargo's
    /// thread pool race them on the singleton `NSPasteboard`.
    #[tokio::test]
    async fn write_entry_round_trips_image_and_text() {
        let clipboard = MacosClipboard::new().expect("init MacosClipboard");

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

        let unsupported = image_entry(vec![0x00, 0x01, 0x02], "image/webp");
        let err = clipboard
            .write_entry(&unsupported)
            .await
            .expect_err("write_entry must reject unsupported image mime types");
        assert!(
            matches!(err, AppError::Unsupported(_)),
            "expected AppError::Unsupported for image/webp, got {err:?}"
        );
    }
}
