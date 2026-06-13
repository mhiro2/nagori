use async_trait::async_trait;
#[cfg(target_os = "macos")]
use nagori_core::RepresentationDataRef;
use nagori_core::{
    AppError, ClipboardContent, ClipboardEntry, Result, StoredClipboardRepresentation,
};
use nagori_platform::{
    ClipboardWriter, clipboard_write_blocking, has_publishable_representation,
    lock_clipboard_for_write, platform_err,
};
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
use objc2_foundation::{NSArray, NSData, NSString};

use super::MacosClipboard;
#[cfg(target_os = "macos")]
use super::{UTI_GIF, UTI_JPEG, UTI_WEBP};

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
pub(super) fn write_pasteboard_items(
    pb: &NSPasteboard,
    items: Vec<Retained<NSPasteboardItem>>,
) -> bool {
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
