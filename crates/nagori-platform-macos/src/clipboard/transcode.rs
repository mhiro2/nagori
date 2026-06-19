#[cfg(target_os = "macos")]
use image::{ImageEncoder, codecs::png::PngEncoder};
#[cfg(target_os = "macos")]
use nagori_core::MAX_DECODED_IMAGE_PIXELS;
#[cfg(target_os = "macos")]
use nagori_core::{AppError, ClipboardData, ClipboardRepresentation};
use nagori_core::{ClipboardSnapshot, ReadBudget, Result};
use nagori_platform::CapturedSnapshot;
#[cfg(target_os = "macos")]
use nagori_platform::{DecodeRgbaError, decode_rgba_with_pixel_cap};
#[cfg(target_os = "macos")]
use objc2_app_kit::{
    NSPasteboard, NSPasteboardTypeHTML, NSPasteboardTypePNG, NSPasteboardTypeRTF,
    NSPasteboardTypeTIFF,
};

#[cfg(target_os = "macos")]
use super::file_url::{FileUrlPaths, collect_file_url_paths, ns_data_to_vec};
#[cfg(target_os = "macos")]
use super::{MAX_IMAGE_REP_BYTES, MAX_TEXT_REP_BYTES};

#[cfg(target_os = "macos")]
pub(super) fn collect_macos_extras(
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

            // Probe the UTF-8 byte length before `to_string()` materialises a
            // Rust `String`, so a multi-GB HTML/RTF payload on the unbounded
            // path is dropped rather than copied (see `text_rep_within_ceiling`).
            if let Some(html) = pb.stringForType(NSPasteboardTypeHTML)
                && text_rep_within_ceiling("text/html", &html)
            {
                out.push(ClipboardRepresentation {
                    mime_type: "text/html".to_owned(),
                    data: ClipboardData::Text(html.to_string()),
                });
            }

            if let Some(rtf) = pb.stringForType(NSPasteboardTypeRTF)
                && text_rep_within_ceiling("application/rtf", &rtf)
            {
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

/// Pre-copy admission check for a pasteboard *text* representation: `true`
/// when its UTF-8 byte length fits under [`MAX_TEXT_REP_BYTES`]. The
/// over-ceiling case is logged (length only, never content) and the
/// representation is skipped — mirrors [`image_rep_within_ceiling`].
#[cfg(target_os = "macos")]
fn text_rep_within_ceiling(mime: &str, string: &objc2_foundation::NSString) -> bool {
    let byte_count = string.len();
    if byte_count > MAX_TEXT_REP_BYTES {
        tracing::warn!(
            mime,
            byte_count,
            ceiling = MAX_TEXT_REP_BYTES,
            "pasteboard_text_rep_exceeds_ceiling"
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
pub(super) fn prepare_tiff_capture(bytes: Vec<u8>) -> Option<(String, Vec<u8>)> {
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
pub(super) fn transcode_tiff_representations(
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
pub(super) async fn transcode_snapshot(
    mut snapshot: ClipboardSnapshot,
) -> Result<ClipboardSnapshot> {
    snapshot.representations = transcode_representations(snapshot.representations).await?;
    Ok(snapshot)
}

#[cfg(not(target_os = "macos"))]
#[allow(clippy::unused_async)]
pub(super) async fn transcode_snapshot(snapshot: ClipboardSnapshot) -> Result<ClipboardSnapshot> {
    Ok(snapshot)
}

/// Normalise a bounded capture after the timed read returns, then re-apply
/// the image budget to the transcoded image.
///
/// The raw pasteboard probe (`oversized_payload`) never sizes TIFF — only
/// PNG / HTML / RTF / text / file URLs — so the normalised image size is
/// first known here. The transcoded bytes are an image (`image/png`), so they
/// answer to `budget.image_bytes`; surfacing an oversize as `Oversized` keeps
/// the pre-read drop semantics the in-timed transcode used to enforce.
#[cfg(target_os = "macos")]
pub(super) async fn finalize_captured(
    captured: CapturedSnapshot,
    budget: ReadBudget,
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
            ClipboardData::Bytes(bytes) if bytes.len() > budget.image_bytes => Some(bytes.len()),
            _ => None,
        })
    {
        return Ok(CapturedSnapshot::Oversized {
            sequence: snapshot.sequence,
            observed_bytes: observed,
            limit: budget.image_bytes,
        });
    }
    Ok(CapturedSnapshot::Captured(snapshot))
}

#[cfg(not(target_os = "macos"))]
#[allow(clippy::unused_async)]
pub(super) async fn finalize_captured(
    captured: CapturedSnapshot,
    _budget: ReadBudget,
) -> Result<CapturedSnapshot> {
    Ok(captured)
}
