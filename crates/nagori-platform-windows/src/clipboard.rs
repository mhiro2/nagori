use std::borrow::Cow;
use std::io::Cursor;
use std::sync::{Arc, Mutex};

use arboard::{Clipboard, ImageData};
use async_trait::async_trait;
use image::{ImageFormat, ImageReader};
use nagori_core::{
    AppError, ClipboardContent, ClipboardData, ClipboardEntry, ClipboardRepresentation,
    ClipboardSequence, ClipboardSnapshot, MAX_DECODED_IMAGE_PIXELS, RepresentationDataRef, Result,
    StoredClipboardRepresentation,
};
use nagori_platform::{
    CapturedSnapshot, ClipboardReader, ClipboardWriter, clipboard_blocking,
    clipboard_write_blocking,
};
use time::OffsetDateTime;

/// Windows clipboard adapter.
///
/// The Win32 clipboard is a process-wide singleton guarded by
/// `OpenClipboard` / `CloseClipboard`. arboard already performs that dance
/// for text reads/writes; we still keep the same `Arc<Mutex<Clipboard>>`
/// pattern as the macOS adapter so a concurrent text-write cannot race a
/// text-read that's about to combine with a separate
/// `GetClipboardSequenceNumber` probe and produce a torn snapshot.
pub struct WindowsClipboard {
    clipboard: Arc<Mutex<Clipboard>>,
}

impl WindowsClipboard {
    pub fn new() -> Result<Self> {
        Ok(Self {
            clipboard: Arc::new(Mutex::new(
                Clipboard::new().map_err(|err| platform_err(&err))?,
            )),
        })
    }
}

#[async_trait]
impl ClipboardReader for WindowsClipboard {
    async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
        // Win32 clipboard reads are synchronous and acquire the global
        // clipboard lock. A misbehaving foreground app can hold that lock
        // for tens of ms, so hop to a blocking thread for the same reasons
        // the macOS adapter does.
        //
        // text (arboard) and CF_HDROP are read under separate
        // `OpenClipboard` / `CloseClipboard` sessions, so without an
        // external check a writer that flips the clipboard between the
        // two reads can produce a torn snapshot (old text paired with new
        // file list). `GetClipboardSequenceNumber` bumps on every
        // clipboard change and is documented thread-safe, so we sample
        // it before and after the reads; if the value drifted we retry
        // up to `MAX_RETRIES` times before giving up and accepting the
        // last attempt. The retry bound prevents an infinite loop if a
        // process is steadily flooding the clipboard.
        let clipboard = self.clipboard.clone();
        clipboard_blocking("current_snapshot", move || -> Result<ClipboardSnapshot> {
            match capture_snapshot(&clipboard, None)? {
                CapturedSnapshot::Captured(snapshot) => Ok(snapshot),
                CapturedSnapshot::Oversized { .. } => unreachable!("unbounded capture cannot skip"),
            }
        })
        .await
        .map_err(|err| AppError::Platform(err.to_string()))?
    }

    async fn current_sequence(&self) -> Result<ClipboardSequence> {
        // `GetClipboardSequenceNumber` is documented thread-safe and does
        // not need `OpenClipboard`. We still route through the blocking
        // pool for consistency with `current_snapshot`.
        clipboard_blocking("current_sequence", || {
            ClipboardSequence::native(i64::from(native_sequence_number()))
        })
        .await
        .map_err(|err| AppError::Platform(err.to_string()))
    }

    async fn current_snapshot_with_max(&self, max_bytes: usize) -> Result<CapturedSnapshot> {
        let clipboard = self.clipboard.clone();
        clipboard_blocking("current_snapshot_with_max", move || {
            capture_snapshot(&clipboard, Some(max_bytes))
        })
        .await
        .map_err(|err| AppError::Platform(err.to_string()))?
    }
}

#[async_trait]
impl ClipboardWriter for WindowsClipboard {
    async fn write_entry(&self, entry: &ClipboardEntry) -> Result<()> {
        if let ClipboardContent::Image(image) = &entry.content {
            let bytes = image.pending_bytes.clone().ok_or_else(|| {
                AppError::Platform(
                    "image payload bytes were not loaded before clipboard write".to_owned(),
                )
            })?;
            return self.write_image_bytes(bytes).await;
        }
        if let ClipboardContent::FileList(files) = &entry.content {
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
        // Pre-scan before touching the clipboard so an entry whose stored
        // reps all sit outside the Windows publisher's mapping table falls
        // back through `write_entry` instead of issuing an `EmptyClipboard`
        // followed by zero `SetClipboardData` calls. Matches the macOS /
        // Linux Wayland contract: only when at least one rep is publishable
        // do we go down the multi-rep path.
        if representations.is_empty() || !has_publishable_representation(representations) {
            return self.write_entry(entry).await;
        }
        #[cfg(windows)]
        {
            // Decode the image reps to their `CF_DIBV5` payloads off the
            // OS-hang timeout path (same rationale as `write_image_bytes`):
            // image decode is the only CPU/memory-bound step here and it
            // touches neither the clipboard mutex nor the Win32 clipboard.
            // `render_dibv5_payloads` returns one slot per rep so the publish
            // step can pair each image rep with its pre-decoded bitmap instead
            // of decoding under the timeout / lock. `reps` is threaded back out
            // so the publish step reuses it without a second clone.
            let reps = representations.to_vec();
            let (reps, dibv5) = tokio::task::spawn_blocking(
                move || -> Result<(Vec<StoredClipboardRepresentation>, Vec<Option<Vec<u8>>>)> {
                    let dibv5 = win::render_dibv5_payloads(&reps)?;
                    Ok((reps, dibv5))
                },
            )
            .await
            .map_err(|err| AppError::Platform(err.to_string()))??;

            let clipboard = self.clipboard.clone();
            clipboard_write_blocking("write_representations", move || -> Result<()> {
                // Hold the arboard mutex across the entire OpenClipboard +
                // EmptyClipboard + N × SetClipboardData batch so a concurrent
                // text-write through arboard cannot land between our
                // EmptyClipboard and the last SetClipboardData call and wipe
                // a partial offer. Only the cheap HGLOBAL copies + Win32
                // publish run here — the image decode already happened above.
                let _guard = clipboard.lock().map_err(|err| lock_err(&err))?;
                win::write_multi_rep(&reps, &dibv5)
            })
            .await
            .map_err(|err| AppError::Platform(err.to_string()))?
        }
        #[cfg(not(windows))]
        {
            let _ = representations;
            Err(AppError::Unsupported(
                "Windows multi-representation writes are Windows-only".to_owned(),
            ))
        }
    }

    async fn write_representation_exact(
        &self,
        representation: &StoredClipboardRepresentation,
    ) -> Result<()> {
        // Strict single-representation paste: refuse a MIME this adapter
        // cannot publish rather than falling back to the primary the way
        // `write_representations` does. `win::write_multi_rep` empties the
        // clipboard and publishes exactly the reps it is handed, so a
        // one-rep batch puts only the chosen format on the clipboard.
        if !has_publishable_representation(std::slice::from_ref(representation)) {
            return Err(AppError::Unsupported(
                "representation cannot be published to the Windows clipboard".to_owned(),
            ));
        }
        #[cfg(windows)]
        {
            // Decode any image rep to its CF_DIBV5 payload off the clipboard
            // mutex / timeout path, exactly as `write_representations` does.
            let reps = vec![representation.clone()];
            let (reps, dibv5) = tokio::task::spawn_blocking(
                move || -> Result<(Vec<StoredClipboardRepresentation>, Vec<Option<Vec<u8>>>)> {
                    let dibv5 = win::render_dibv5_payloads(&reps)?;
                    Ok((reps, dibv5))
                },
            )
            .await
            .map_err(|err| AppError::Platform(err.to_string()))??;

            let clipboard = self.clipboard.clone();
            clipboard_write_blocking("write_representation_exact", move || -> Result<()> {
                let _guard = clipboard.lock().map_err(|err| lock_err(&err))?;
                win::write_multi_rep(&reps, &dibv5)
            })
            .await
            .map_err(|err| AppError::Platform(err.to_string()))?
        }
        #[cfg(not(windows))]
        {
            Err(AppError::Unsupported(
                "Windows multi-representation writes are Windows-only".to_owned(),
            ))
        }
    }
}

/// True when at least one stored rep has a known Windows mapping.
///
/// Pre-scan used by `write_representations` so an entry whose stored reps
/// are all outside the publisher's table (e.g. only `application/json`
/// without a plain fallback) falls back to `write_entry` instead of
/// issuing an `EmptyClipboard` for nothing. The body inspects only
/// `nagori-core` types so it stays target-independent — the workspace
/// builds every platform crate on every host and this helper has to
/// resolve on non-Windows targets too.
fn has_publishable_representation(reps: &[StoredClipboardRepresentation]) -> bool {
    reps.iter()
        .any(|rep| match (rep.mime_type.as_str(), &rep.data) {
            (
                "text/plain" | "text/html" | "application/rtf",
                RepresentationDataRef::InlineText(_),
            )
            | (
                "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/tiff",
                RepresentationDataRef::DatabaseBlob(_),
            ) => true,
            ("text/uri-list", RepresentationDataRef::FilePaths(paths)) => !paths.is_empty(),
            _ => false,
        })
}

impl WindowsClipboard {
    async fn write_files(&self, paths: Vec<String>) -> Result<()> {
        if paths.is_empty() {
            return Err(AppError::Unsupported(
                "file-list clipboard entry has no paths".to_owned(),
            ));
        }
        let clipboard = self.clipboard.clone();
        clipboard_write_blocking("write_files", move || -> Result<()> {
            // Hold the arboard mutex across the whole `OpenClipboard +
            // EmptyClipboard + SetClipboardData(CF_HDROP)` batch so a
            // concurrent text-write through arboard cannot land between
            // our `EmptyClipboard` call (which would wipe our CF_HDROP
            // offer) and `SetClipboardData`.
            let _guard = clipboard.lock().map_err(|err| lock_err(&err))?;
            #[cfg(windows)]
            {
                win::write_file_list(&paths)
            }
            #[cfg(not(windows))]
            {
                let _ = paths;
                Err(AppError::Unsupported(
                    "Windows file-list writes are Windows-only".to_owned(),
                ))
            }
        })
        .await
        .map_err(|err| AppError::Platform(err.to_string()))?
    }

    async fn write_image_bytes(&self, bytes: Vec<u8>) -> Result<()> {
        // arboard publishes images on Windows as `CF_DIBV5`, so callers must
        // hand us decoded RGBA. The capture path stores encoded bytes
        // (image/png from this adapter, image/{tiff,jpeg,gif,webp} from
        // macOS sessions paste-restored on Windows) and `image` auto-detects
        // the format.
        //
        // The decode runs on a plain blocking task *outside*
        // `CLIPBOARD_OP_TIMEOUT`: it is CPU/memory-bound (bounded by
        // `MAX_DECODED_IMAGE_PIXELS`), touches neither the clipboard mutex nor
        // the Win32 clipboard, and so is not the OS-hang the timeout guards
        // against. Keeping it out of the timed section avoids a false timeout
        // on a large-but-valid image and — critically — stops a detached
        // decode task from landing a stale `SetClipboardData` after the caller
        // already saw a timeout error.
        let image_data = tokio::task::spawn_blocking(move || -> Result<ImageData<'static>> {
            // Probe dimensions first so an encoded payload whose advertised
            // canvas blows past `MAX_DECODED_IMAGE_PIXELS` (e.g. a 1 KB PNG
            // claiming 65535×65535) is rejected before `decode` allocates a
            // multi-GB RGBA buffer.
            reject_oversized_image(&bytes)?;
            let rgba = ImageReader::new(Cursor::new(&bytes))
                .with_guessed_format()
                .map_err(|err| AppError::Platform(format!("image probe failed: {err}")))?
                .decode()
                .map_err(|err| AppError::Platform(format!("image decode failed: {err}")))?
                .to_rgba8();
            let (width, height) = rgba.dimensions();
            Ok(ImageData {
                width: width as usize,
                height: height as usize,
                bytes: Cow::Owned(rgba.into_raw()),
            })
        })
        .await
        .map_err(|err| AppError::Platform(err.to_string()))??;

        // The image decode above ran on a plain blocking task (CPU/memory
        // bound, no clipboard lock). The actual Win32 clipboard write awaits
        // to completion without a timeout: a timed-out `set_image` cannot be
        // cancelled and would clobber newer clipboard content on late return.
        let clipboard = self.clipboard.clone();
        clipboard_write_blocking("write_image_bytes", move || -> Result<()> {
            clipboard
                .lock()
                .map_err(|err| lock_err(&err))?
                .set_image(image_data)
                .map_err(|err| platform_err(&err))
        })
        .await
        .map_err(|err| AppError::Platform(err.to_string()))?
    }
}

/// Reject an encoded image payload whose decoded canvas would exceed
/// [`MAX_DECODED_IMAGE_PIXELS`].
///
/// `image::ImageReader::into_dimensions` reads only the format header (e.g.
/// PNG's IHDR, JPEG's SOF) so the probe stays bounded even when the encoded
/// payload itself is multiple MB. The error path matches the rest of the
/// adapter: callers receive an `AppError::Unsupported` and report the
/// rejection upward instead of crashing on the subsequent `decode`.
///
/// A probe failure (corrupt header, unknown format) returns `Ok(())` so the
/// downstream `decode` call still gets to produce the more descriptive
/// platform error; it is the dimensions-exceed-cap case that needs the
/// early bail-out.
fn reject_oversized_image(bytes: &[u8]) -> Result<()> {
    let Some(pixels) = image_pixel_count_from_encoded(bytes) else {
        return Ok(());
    };
    if pixels > MAX_DECODED_IMAGE_PIXELS {
        return Err(AppError::Unsupported(format!(
            "image dimensions {pixels} pixels exceed MAX_DECODED_IMAGE_PIXELS ({MAX_DECODED_IMAGE_PIXELS})"
        )));
    }
    Ok(())
}

/// Compute the decoded pixel count of an encoded image header.
///
/// Returns `None` when the prefix is too short to identify a format or when
/// `into_dimensions` fails for any reason; the only contract is that a
/// successful return reflects the dimensions the corresponding `decode`
/// call would observe.
fn image_pixel_count_from_encoded(bytes: &[u8]) -> Option<u64> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let (width, height) = reader.into_dimensions().ok()?;
    Some(u64::from(width).saturating_mul(u64::from(height)))
}

/// Pixel count parsed from the leading bytes of a `BITMAPINFOHEADER` /
/// `BITMAPV5HEADER`.
///
/// Both headers share the same first 12 bytes: `biSize` (u32, offset 0),
/// `biWidth` (i32, offset 4), `biHeight` (i32, offset 8). `biHeight` is
/// signed because a top-down DIB encodes scan-order in its sign, so the
/// pixel count uses the absolute values of both axes. Returns `None` when
/// the prefix is shorter than 12 bytes.
#[cfg(any(windows, test))]
fn dib_pixel_count_from_header(bytes: &[u8]) -> Option<u64> {
    if bytes.len() < 12 {
        return None;
    }
    let width = i32::from_le_bytes(bytes[4..8].try_into().ok()?);
    let height = i32::from_le_bytes(bytes[8..12].try_into().ok()?);
    Some(u64::from(width.unsigned_abs()).saturating_mul(u64::from(height.unsigned_abs())))
}

/// Pixel count parsed directly from a PNG's IHDR chunk.
///
/// `image::ImageReader::into_dimensions` advances the PNG decoder until it
/// finds IDAT, which means a real PNG with ancillary chunks (gAMA, sRGB,
/// pHYs, …) sitting between IHDR and IDAT would need an unbounded prefix
/// to probe — and a 64-byte prefix would silently return `None`, letting
/// an oversized PNG slip past the capture probe and into arboard's
/// unbounded `read_png` allocation.
///
/// The PNG spec (RFC 2083 §3.2, §4.1.1) mandates IHDR is the first chunk
/// and that its layout is fixed: signature (8 B) + length=`0x0000_000D`
/// (4 B BE) + type=`"IHDR"` (4 B) + width (4 B BE) + height (4 B BE) + …
/// So reading bytes 0..24 is enough to recover the advertised dimensions
/// even when later chunks are absent or unparseable. Returns `None` on
/// signature / chunk-type mismatch so callers fall through to whatever
/// decode error the platform path produces.
#[cfg(any(windows, test))]
fn png_pixel_count_from_ihdr(bytes: &[u8]) -> Option<u64> {
    // PNG byte layout we rely on (RFC 2083 §3.2, §4.1.1). The IHDR
    // chunk is mandated to be the *first* chunk, with a fixed payload
    // length and shape, so the first 24 bytes of any spec-compliant
    // stream are deterministic:
    //
    //   bytes[0..8]   PNG signature magic (\x89 P N G \r \n \x1a \n)
    //   bytes[8..12]  first chunk length, big-endian u32 — must be 13 for IHDR
    //   bytes[12..16] first chunk type — must be the ASCII "IHDR"
    //   bytes[16..20] IHDR width, big-endian u32
    //   bytes[20..24] IHDR height, big-endian u32
    //
    // We reject anything that breaks this contract rather than try to
    // recover, because the only callers that hit this function are the
    // pixel-cap probes — falling through to `decode()` is safer than
    // returning a fabricated pixel count.
    const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.len() < 24 || bytes[..8] != PNG_SIGNATURE {
        return None;
    }
    let length = u32::from_be_bytes(bytes[8..12].try_into().ok()?);
    if length != 13 || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some(u64::from(width).saturating_mul(u64::from(height)))
}

fn capture_snapshot(
    clipboard: &Mutex<Clipboard>,
    max_bytes: Option<usize>,
) -> Result<CapturedSnapshot> {
    const MAX_RETRIES: usize = 3;
    let mut attempt = 0;
    loop {
        attempt += 1;
        let before = native_sequence_number();
        if let Some(limit) = max_bytes {
            #[cfg(windows)]
            if let Some(observed) = win::oversized_payload(limit) {
                return Ok(CapturedSnapshot::Oversized {
                    sequence: ClipboardSequence::native(i64::from(native_sequence_number())),
                    observed_bytes: observed,
                    limit,
                });
            }
            #[cfg(not(windows))]
            let _ = limit;
        }

        let mut guard = clipboard.lock().map_err(|err| lock_err(&err))?;
        let plain = match guard.get_text() {
            Ok(text) => Some(text),
            Err(arboard::Error::ContentNotAvailable) => None,
            Err(err) => return Err(platform_err(&err)),
        };
        // arboard's `get_image` opens its own `OpenClipboard` session and
        // pulls the raw `CF_DIBV5` (falling back to `CF_DIB`) bytes into a
        // freshly allocated RGBA buffer. Encode to PNG so the rest of the
        // pipeline (storage, search snippets, IPC, copy-back) can treat
        // Windows captures the same way macOS publishes `image/png` straight
        // off the pasteboard. Format unavailability is the common case and
        // surfaces as `ContentNotAvailable`, which we silently skip — only
        // true Win32 failures bubble up as `AppError::Platform`.
        //
        // Probe the published image dimensions before letting arboard
        // materialise an RGBA buffer: arboard's `read_png` /
        // `read_cf_dibv5` allocate `width × height × 4` bytes
        // unconditionally, so a small encoded PNG with pathological
        // dimensions (e.g. 65535×65535) would OOM the daemon long before
        // the post-load byte-budget check runs. Skip the image rep when
        // the cap is exceeded and continue with whatever text / file-list
        // also rode on this snapshot.
        #[cfg(windows)]
        let skip_image = match win::image_pixel_overflow(MAX_DECODED_IMAGE_PIXELS) {
            Some(observed) => {
                tracing::warn!(
                    observed_pixels = observed,
                    max_pixels = MAX_DECODED_IMAGE_PIXELS,
                    "image_rep_dropped reason=decoded_pixels_exceed_cap"
                );
                true
            }
            None => false,
        };
        #[cfg(not(windows))]
        let skip_image = false;
        let image = if skip_image {
            None
        } else {
            match guard.get_image() {
                Ok(img) => Some(img),
                Err(arboard::Error::ContentNotAvailable) => None,
                Err(err) => return Err(platform_err(&err)),
            }
        };
        // Drop the arboard guard before the second Win32 read so we don't hold
        // it across the CF_HDROP OpenClipboard call; the sequence-stability
        // check is what protects us against a write landing in between.
        drop(guard);

        let mut representations = Vec::new();

        #[cfg(windows)]
        if let Some(files) = win::read_file_list() {
            representations.push(ClipboardRepresentation {
                mime_type: "text/uri-list".to_owned(),
                data: ClipboardData::FilePaths(files),
            });
        }

        if let Some(img) = image
            && let Some(png) = encode_rgba_to_png(img)
        {
            representations.push(ClipboardRepresentation {
                mime_type: "image/png".to_owned(),
                data: ClipboardData::Bytes(png),
            });
        }

        if let Some(text) = plain {
            representations.push(ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text(text),
            });
        }

        let after = native_sequence_number();
        if before == after || attempt >= MAX_RETRIES {
            let snapshot = ClipboardSnapshot {
                sequence: ClipboardSequence::native(i64::from(after)),
                captured_at: OffsetDateTime::now_utc(),
                source: None,
                representations,
            };
            if let Some(limit) = max_bytes {
                let observed_bytes = total_payload_bytes(&snapshot);
                if observed_bytes > limit {
                    return Ok(CapturedSnapshot::Oversized {
                        sequence: snapshot.sequence,
                        observed_bytes,
                        limit,
                    });
                }
            }
            return Ok(CapturedSnapshot::Captured(snapshot));
        }
    }
}

fn total_payload_bytes(snapshot: &ClipboardSnapshot) -> usize {
    snapshot
        .representations
        .iter()
        .map(|rep| match &rep.data {
            ClipboardData::Text(text) => text.len(),
            ClipboardData::Bytes(bytes) => bytes.len(),
            ClipboardData::FilePaths(paths) => paths.iter().map(String::len).sum(),
        })
        .sum()
}

fn encode_rgba_to_png(img: ImageData<'_>) -> Option<Vec<u8>> {
    // arboard returns 8-bit RGBA. Width/height come back as `usize` but
    // `image::RgbaImage::from_raw` takes `u32`; reject silently when a
    // pathological clipboard claims dimensions larger than `u32::MAX` (real
    // Win32 bitmaps cannot exceed `LONG`) so the rest of the capture path
    // still yields whatever text / file-list it already collected. Take the
    // `ImageData` by value so we can move its `Cow` buffer straight into
    // `RgbaImage::from_raw` without cloning the (potentially multi-MB) RGBA
    // payload.
    let width = u32::try_from(img.width).ok()?;
    let height = u32::try_from(img.height).ok()?;
    let rgba = image::RgbaImage::from_raw(width, height, img.bytes.into_owned())?;
    let mut buf = Vec::new();
    rgba.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)
        .ok()?;
    Some(buf)
}

#[cfg(windows)]
fn native_sequence_number() -> u32 {
    // SAFETY: GetClipboardSequenceNumber takes no arguments and is
    // documented thread-safe; it returns the current process-visible
    // change counter as a DWORD.
    unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() }
}

#[cfg(not(windows))]
const fn native_sequence_number() -> u32 {
    // Off-target builds (e.g. running `cargo check` on macOS for the
    // workspace) compile this crate too. Return a constant so the
    // before/after sequence comparison in `current_snapshot` short
    // circuits cleanly and the loop terminates on the first attempt;
    // the daemon never actually runs on non-Windows hosts.
    0
}

#[cfg(windows)]
mod win {
    use std::ffi::OsString;
    use std::io::Cursor;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::{char, mem, slice};

    use image::ImageReader;
    use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, TRUE};
    use windows_sys::Win32::Graphics::Gdi::{BI_BITFIELDS, BITMAPV5HEADER, LCS_GM_IMAGES};
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
        OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
    };
    use windows_sys::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT};
    use windows_sys::Win32::UI::Shell::{DROPFILES, DragQueryFileW};

    use nagori_core::{AppError, RepresentationDataRef, Result, StoredClipboardRepresentation};

    /// Sentinel value documented for `DragQueryFileW`: when `iFile == 0xFFFFFFFF`,
    /// the function returns the file count instead of writing a path.
    const DRAG_QUERY_COUNT: u32 = 0xFFFF_FFFF;

    /// Upper bound on the number of paths we will read from a single
    /// `CF_HDROP`. The Windows shell itself caps Explorer drag-and-drop
    /// at far fewer entries; a payload pretending to carry millions of
    /// paths is either corrupt or malicious. Capping here prevents a
    /// rogue process from steering us into a multi-GB allocation just
    /// by writing a crafted `DROPFILES` blob to the clipboard.
    const MAX_PATHS: u32 = 4096;

    /// Win32 long-path limit (32,767 wchars) plus a terminator. Any
    /// `DragQueryFileW` length probe that exceeds this is either a
    /// corrupt `DROPFILES` payload or an attempt at oversized
    /// allocation; we skip that path rather than honour the length.
    const MAX_PATH_WCHARS: u32 = 32_768;

    /// RAII guard that releases the per-thread clipboard lock on drop.
    ///
    /// Without this, a panic between `OpenClipboard` and the explicit
    /// `CloseClipboard()` call would leave the clipboard pinned by the
    /// daemon thread, blocking every other app on the system until the
    /// process exits. The bounded allocations above make a panic very
    /// unlikely, but `Vec::with_capacity` / `vec![..]` can still abort
    /// the process on OOM and we don't want to be the thread holding
    /// the lock when that happens.
    struct ClipboardGuard;

    impl Drop for ClipboardGuard {
        fn drop(&mut self) {
            // SAFETY: this guard is only constructed after a successful
            // `OpenClipboard`, so a matching `CloseClipboard` is safe.
            unsafe {
                CloseClipboard();
            }
        }
    }

    /// Peek the image rep on the clipboard and return the decoded pixel
    /// count when it exceeds `max_pixels`.
    ///
    /// arboard's `get_image` decodes whichever format it finds in PNG →
    /// `CF_DIBV5` → `CF_DIB` order and then allocates a `width × height × 4`
    /// RGBA buffer, so an attacker-controlled small PNG with huge advertised
    /// dimensions would OOM the daemon long before the post-load byte check.
    /// This probe mirrors arboard's lookup order *and stops at the first
    /// format that's available* — checking later formats once the winning
    /// one is safe would incorrectly drop a safe PNG just because a stale
    /// oversized `CF_DIBV5` sits alongside it.
    ///
    /// PNG dimensions are read from the IHDR chunk directly (24-byte
    /// prefix) rather than through `image::ImageReader::into_dimensions`,
    /// because the latter advances to IDAT and a real PNG with ancillary
    /// chunks before IDAT would silently return `None` from a 64-byte
    /// peek, letting an oversized payload through. DIB / DIBV5 dimensions
    /// come from the 12-byte `BITMAPINFOHEADER` prefix that both header
    /// variants share.
    ///
    /// Returns `None` when no image rep is present, when the winning
    /// format's dimensions fit under the cap, or when its probe fails —
    /// the daemon then proceeds with the regular capture path so a
    /// malformed header surfaces as an arboard error rather than a silent
    /// skip.
    pub(super) fn image_pixel_overflow(max_pixels: u64) -> Option<u64> {
        // SAFETY: every successful `OpenClipboard` is paired with the
        // `ClipboardGuard` drop path. `GetClipboardData` handles are borrowed
        // from the OS-owned clipboard and are only inspected while the
        // clipboard remains open.
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return None;
            }
            let _guard = ClipboardGuard;
            if let Some(png_id) = png_format_id()
                && IsClipboardFormatAvailable(png_id) != 0
            {
                return png_pixel_count(png_id).filter(|pixels| *pixels > max_pixels);
            }
            for format in [CF_DIBV5, CF_DIB] {
                if IsClipboardFormatAvailable(u32::from(format)) != 0 {
                    return dib_pixel_count(u32::from(format))
                        .filter(|pixels| *pixels > max_pixels);
                }
            }
            None
        }
    }

    /// Copy out at most `max_len` bytes from the clipboard handle's
    /// `HGLOBAL` so a small prefix can be fed to the pure parsing
    /// helpers without holding the clipboard lock across the parse.
    ///
    /// Returns `None` for null handles, empty buffers, or any `GlobalLock`
    /// failure — the caller then continues with the regular capture path
    /// and a downstream arboard error surfaces the underlying issue.
    unsafe fn copy_clipboard_prefix(format: u32, max_len: usize) -> Option<Vec<u8>> {
        let handle = unsafe { GetClipboardData(format) };
        if handle.is_null() {
            return None;
        }
        let size = unsafe { GlobalSize(handle) };
        if size == 0 {
            return None;
        }
        let locked = unsafe { GlobalLock(handle) };
        if locked.is_null() {
            return None;
        }
        let prefix_len = size.min(max_len);
        let mut prefix = vec![0u8; prefix_len];
        unsafe {
            std::ptr::copy_nonoverlapping(locked.cast::<u8>(), prefix.as_mut_ptr(), prefix_len);
            let _ = GlobalUnlock(handle);
        }
        Some(prefix)
    }

    /// Read the PNG's width × height directly from the IHDR chunk.
    ///
    /// PNG's signature is 8 bytes and IHDR's `length + type + payload`
    /// fields occupy the next 16 bytes (length=13, type="IHDR", then
    /// width / height u32 BE). 24 bytes is therefore the exact prefix
    /// needed; we copy 32 to absorb any host quirks without paying for
    /// the rest of the blob.
    unsafe fn png_pixel_count(format: u32) -> Option<u64> {
        let prefix = unsafe { copy_clipboard_prefix(format, 32) }?;
        super::png_pixel_count_from_ihdr(&prefix)
    }

    /// Read the DIB / DIBV5 `biWidth` / `biHeight` directly from the
    /// `BITMAPINFOHEADER` prefix. Both `BITMAPINFOHEADER` and
    /// `BITMAPV5HEADER` start with `biSize` (offset 0, u32), `biWidth`
    /// (offset 4, i32), `biHeight` (offset 8, i32) — so the same 12-byte
    /// peek works for either header layout.
    unsafe fn dib_pixel_count(format: u32) -> Option<u64> {
        let prefix = unsafe { copy_clipboard_prefix(format, 12) }?;
        super::dib_pixel_count_from_header(&prefix)
    }

    pub(super) fn oversized_payload(max_bytes: usize) -> Option<usize> {
        // SAFETY: every successful `OpenClipboard` is paired with the
        // `ClipboardGuard` drop path. `GetClipboardData` handles are borrowed
        // from the OS-owned clipboard and are only inspected while the
        // clipboard remains open.
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return None;
            }
            let _guard = ClipboardGuard;
            let mut observed = 0_usize;
            if IsClipboardFormatAvailable(u32::from(CF_UNICODETEXT)) != 0
                && let Some(text_bytes) = unicode_text_utf8_len()
            {
                observed = observed.saturating_add(text_bytes);
                if observed > max_bytes {
                    return Some(observed);
                }
            }
            if IsClipboardFormatAvailable(u32::from(CF_HDROP)) != 0
                && let Some(file_list_bytes) = global_data_size(u32::from(CF_HDROP))
            {
                observed = observed.saturating_add(file_list_bytes);
                if observed > max_bytes {
                    return Some(observed);
                }
            }
            // Skip CF_DIB / CF_DIBV5 here. Raw DIB is uncompressed
            // (~width*height*4 bytes) and routinely several MiB for
            // ordinary screenshots that fit comfortably under the entry
            // cap once we RGBA -> PNG encode in `capture_snapshot`. The
            // post-encode `total_payload_bytes` check is the authoritative
            // limit, and `image_pixel_overflow` still rejects pathological
            // dimensions before the RGBA allocation. The registered "PNG"
            // format, however, is *already* encoded, so its raw size is a
            // truthful preview of what will land in storage — keep that
            // probe so a small-dimensioned but multi-MB PNG bails out
            // before arboard reads the full payload into an RGBA buffer.
            if let Some(png_id) = png_format_id()
                && IsClipboardFormatAvailable(png_id) != 0
                && let Some(png_bytes) = global_data_size(png_id)
            {
                observed = observed.saturating_add(png_bytes);
                if observed > max_bytes {
                    return Some(observed);
                }
            }
            None
        }
    }

    /// Register the `"PNG"` clipboard format name and return its
    /// session-stable id. Registering the same name twice returns the
    /// same id, so calling this every probe is cheap. A registration
    /// failure (out of clipboard-format slots) is treated as "no PNG
    /// row" — the `CF_DIBV5` / `CF_DIB` fallback still runs.
    unsafe fn png_format_id() -> Option<u32> {
        // "PNG" as a NUL-terminated UTF-16 literal.
        const PNG_NAME: [u16; 4] = [b'P' as u16, b'N' as u16, b'G' as u16, 0];
        let id = unsafe { RegisterClipboardFormatW(PNG_NAME.as_ptr()) };
        (id != 0).then_some(id)
    }

    unsafe fn global_data_size(format: u32) -> Option<usize> {
        let handle = unsafe { GetClipboardData(format) };
        if handle.is_null() {
            return None;
        }
        let bytes = unsafe { GlobalSize(handle) };
        (bytes > 0).then_some(bytes)
    }

    unsafe fn unicode_text_utf8_len() -> Option<usize> {
        let handle = unsafe { GetClipboardData(u32::from(CF_UNICODETEXT)) };
        if handle.is_null() {
            return None;
        }
        let bytes = unsafe { GlobalSize(handle) };
        if bytes < mem::size_of::<u16>() {
            return Some(0);
        }
        let locked = unsafe { GlobalLock(handle) };
        if locked.is_null() {
            return None;
        }
        let units = bytes / mem::size_of::<u16>();
        let wide = unsafe { slice::from_raw_parts(locked.cast::<u16>(), units) };
        let text_units = wide.iter().position(|unit| *unit == 0).unwrap_or(units);
        let utf8_len = char::decode_utf16(wide[..text_units].iter().copied())
            .map(|decoded| decoded.unwrap_or(char::REPLACEMENT_CHARACTER).len_utf8())
            .sum();
        let _ = unsafe { GlobalUnlock(handle) };
        Some(utf8_len)
    }

    /// Read the `CF_HDROP` representation from the system clipboard, if
    /// present. Returns paths as UTF-8 strings; non-UTF-8 paths (lone
    /// surrogates from filesystems that allow them) are skipped because
    /// the daemon's domain model is `String`, not `OsString`.
    pub(super) fn read_file_list() -> Option<Vec<String>> {
        // SAFETY: every Win32 call below is paired with its release.
        // `OpenClipboard(null)` attaches to the calling thread; the
        // `ClipboardGuard` RAII handle calls `CloseClipboard` on every
        // return path, including panics. `HDROP` is documented to be the
        // handle value returned by `GetClipboardData(CF_HDROP)` directly
        // — no `GlobalLock`/`Unlock` dance is required (and using the
        // locked pointer where `HDROP` is expected is incorrect:
        // `DragQueryFileW` would dereference data that doesn't match the
        // documented `DROPFILES` header layout).
        unsafe {
            if IsClipboardFormatAvailable(u32::from(CF_HDROP)) == 0 {
                return None;
            }
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return None;
            }
            let _guard = ClipboardGuard;
            let handle = GetClipboardData(u32::from(CF_HDROP));
            if handle.is_null() {
                return None;
            }
            let hdrop = handle.cast();
            let raw_count = DragQueryFileW(hdrop, DRAG_QUERY_COUNT, std::ptr::null_mut(), 0);
            // Trust the OS but verify the count: a malicious sender can
            // hand us an attacker-controlled `DROPFILES` blob, and we'd
            // otherwise honour any 32-bit count with a `Vec::with_capacity`.
            let count = raw_count.min(MAX_PATHS);
            let mut out = Vec::with_capacity(count as usize);
            for index in 0..count {
                // First call with null buffer returns the required length
                // in TCHARs, *excluding* the terminating null.
                let needed = DragQueryFileW(hdrop, index, std::ptr::null_mut(), 0);
                if needed == 0 || needed > MAX_PATH_WCHARS {
                    // Either no path is present at this index or the
                    // length blows past Win32's long-path cap; skip
                    // rather than serve an attacker-controlled allocation.
                    continue;
                }
                // Buffer holds `needed` wchars plus the terminating NUL;
                // track capacity as u32 so we never widen-then-narrow back
                // through `as` and trip the truncation lint.
                let cap_u32 = needed.saturating_add(1);
                let mut buf = vec![0u16; cap_u32 as usize];
                let written = DragQueryFileW(hdrop, index, buf.as_mut_ptr(), cap_u32);
                if written == 0 {
                    continue;
                }
                buf.truncate(written as usize);
                let os = OsString::from_wide(&buf);
                if let Some(s) = os.to_str() {
                    out.push(s.to_owned());
                }
            }
            // `_guard` releases the clipboard on scope exit.
            if out.is_empty() { None } else { Some(out) }
        }
    }

    /// Publish a list of filesystem paths as `CF_HDROP`.
    ///
    /// The Win32 clipboard expects a `HGLOBAL` allocated with
    /// `GMEM_MOVEABLE` whose contents are a `DROPFILES` header followed
    /// by a wide-character path buffer terminated by a double NUL. We
    /// own the allocation up to the point `SetClipboardData` succeeds —
    /// from there the OS takes ownership and we must NOT free it. On
    /// any earlier failure we explicitly `GlobalFree` so a partial path
    /// publish does not leak the allocation.
    pub(super) fn write_file_list(paths: &[String]) -> Result<()> {
        let handle = prepare_cf_hdrop(paths)?;
        publish_handles(&[(u32::from(CF_HDROP), handle)])
    }

    /// Publish multiple stored representations atomically.
    ///
    /// Allocates one `HGLOBAL` per mappable rep (and, for `image/png`,
    /// two — the registered "PNG" payload plus a `CF_DIBV5` companion so
    /// Word-class targets that ignore "PNG" still receive a bitmap), then
    /// opens the clipboard once, calls `EmptyClipboard`, and walks the
    /// pre-allocated handle list publishing each format. Building every
    /// `HGLOBAL` before touching the clipboard means a decode error
    /// (e.g. an unreadable PNG blob) surfaces before we clear the user's
    /// previous selection — matching the macOS adapter's pre-scan
    /// guarantee.
    /// Decode every image rep to its `CF_DIBV5` payload, returning one slot
    /// per input rep (image reps → `Some(dibv5_bytes)`, every other rep and
    /// empty image blobs → `None`).
    ///
    /// Split out of [`prepare_one_rep`] so the CPU/memory-bound decode runs
    /// off the `CLIPBOARD_OP_TIMEOUT` path — see the caller in
    /// `write_representations`. The slots are positionally aligned with `reps`
    /// so the publish step can pair each image rep with its bitmap.
    pub(super) fn render_dibv5_payloads(
        reps: &[StoredClipboardRepresentation],
    ) -> Result<Vec<Option<Vec<u8>>>> {
        reps.iter()
            .map(|rep| match (rep.mime_type.as_str(), &rep.data) {
                (
                    "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/tiff",
                    RepresentationDataRef::DatabaseBlob(bytes),
                ) if !bytes.is_empty() => Ok(Some(build_dibv5_payload(bytes)?)),
                _ => Ok(None),
            })
            .collect()
    }

    pub(super) fn write_multi_rep(
        reps: &[StoredClipboardRepresentation],
        dibv5: &[Option<Vec<u8>>],
    ) -> Result<()> {
        let handles = prepare_handles_for_reps(reps, dibv5)?;
        if handles.is_empty() {
            // Caller pre-scanned; reaching this branch means every rep
            // dropped through to `_ => {}` between the pre-scan and now,
            // which can only happen if the rep set changed shape under
            // us. Surface the platform error rather than issue an
            // `EmptyClipboard` for nothing.
            return Err(AppError::Platform(
                "no representable bytes for Windows multi-rep publish".to_owned(),
            ));
        }
        publish_handles(&handles)
    }

    /// Allocate every `(format, HGLOBAL)` pair for the rep batch.
    ///
    /// All handles are built before the clipboard is touched so any
    /// allocation / decode error tears down the partial allocation
    /// list cleanly (via `GlobalFree`) instead of leaking. Duplicate
    /// formats from a malformed rep set are coalesced: only the first
    /// occurrence wins, subsequent duplicates are freed in place.
    fn prepare_handles_for_reps(
        reps: &[StoredClipboardRepresentation],
        dibv5: &[Option<Vec<u8>>],
    ) -> Result<Vec<(u32, HANDLE)>> {
        let mut acquired: Vec<(u32, HANDLE)> = Vec::new();
        let result = (|| -> Result<()> {
            for (index, rep) in reps.iter().enumerate() {
                let rendered = dibv5.get(index).and_then(Option::as_deref);
                prepare_one_rep(rep, rendered, &mut acquired)?;
            }
            Ok(())
        })();
        if let Err(err) = result {
            // Free every handle we already acquired before bubbling
            // the error out — none have been handed to the OS yet.
            for (_, handle) in &acquired {
                // SAFETY: handles in `acquired` came from `GlobalAlloc`
                // and have not been transferred via `SetClipboardData`.
                unsafe { GlobalFree(*handle) };
            }
            return Err(err);
        }
        Ok(acquired)
    }

    /// Push the `(format, HGLOBAL)` for one rep into `acquired`.
    /// Duplicates of an already-acquired format are freed in place so
    /// a malformed input with two `text/plain` reps doesn't publish
    /// two `CF_UNICODETEXT` handles (the second `SetClipboardData`
    /// would win, leaking the first allocation).
    fn prepare_one_rep(
        rep: &StoredClipboardRepresentation,
        rendered_dibv5: Option<&[u8]>,
        acquired: &mut Vec<(u32, HANDLE)>,
    ) -> Result<()> {
        match (rep.mime_type.as_str(), &rep.data) {
            ("text/plain", RepresentationDataRef::InlineText(text)) => {
                push_handle(
                    acquired,
                    u32::from(CF_UNICODETEXT),
                    prepare_cf_unicode_text(text)?,
                );
            }
            ("text/html", RepresentationDataRef::InlineText(text)) => {
                let format_id = register_format("HTML Format").ok_or_else(|| {
                    AppError::Platform(
                        "RegisterClipboardFormatW(\"HTML Format\") failed".to_owned(),
                    )
                })?;
                push_handle(acquired, format_id, prepare_cf_html(text)?);
            }
            ("application/rtf", RepresentationDataRef::InlineText(text)) => {
                let format_id = register_format("Rich Text Format").ok_or_else(|| {
                    AppError::Platform(
                        "RegisterClipboardFormatW(\"Rich Text Format\") failed".to_owned(),
                    )
                })?;
                push_handle(acquired, format_id, prepare_byte_buffer(text.as_bytes())?);
            }
            ("image/png", RepresentationDataRef::DatabaseBlob(bytes)) => {
                if bytes.is_empty() {
                    return Ok(());
                }
                // The `CF_DIBV5` companion (BGRA bottom-up) was decoded
                // up-front by `render_dibv5_payloads`; the registered "PNG"
                // format ships the raw PNG bytes as-is. A non-empty image rep
                // always has a `Some` slot, so a `None` here means the render
                // / publish slices fell out of sync — fail loudly rather than
                // silently drop the image.
                let dibv5 = rendered_dibv5.ok_or_else(|| {
                    AppError::Platform(
                        "missing pre-rendered CF_DIBV5 payload for image/png rep".to_owned(),
                    )
                })?;
                push_handle(acquired, u32::from(CF_DIBV5), prepare_byte_buffer(dibv5)?);
                if let Some(png_id) = register_format("PNG") {
                    push_handle(acquired, png_id, prepare_byte_buffer(bytes)?);
                }
            }
            (
                "image/jpeg" | "image/gif" | "image/webp" | "image/tiff",
                RepresentationDataRef::DatabaseBlob(bytes),
            ) => {
                if bytes.is_empty() {
                    return Ok(());
                }
                // Non-PNG image formats lack a stable registered
                // clipboard format on Windows, so we only publish a
                // `CF_DIBV5` rendering — pre-decoded by
                // `render_dibv5_payloads`. The pixel data is the decoded
                // source, which is what Word / Paint pull from
                // `CF_DIBV5` anyway.
                let dibv5 = rendered_dibv5.ok_or_else(|| {
                    AppError::Platform(
                        "missing pre-rendered CF_DIBV5 payload for image rep".to_owned(),
                    )
                })?;
                push_handle(acquired, u32::from(CF_DIBV5), prepare_byte_buffer(dibv5)?);
            }
            ("text/uri-list", RepresentationDataRef::FilePaths(paths)) if !paths.is_empty() => {
                push_handle(acquired, u32::from(CF_HDROP), prepare_cf_hdrop(paths)?);
            }
            _ => {
                // The pre-scan guarantees at least one mappable rep
                // exists; drop unsupported entries silently so
                // unfamiliar future MIMEs do not block the publish of
                // the ones we already understand.
            }
        }
        Ok(())
    }

    /// Append `(format, handle)` to `acquired`, freeing the handle in
    /// place if `format` is already present.
    fn push_handle(acquired: &mut Vec<(u32, HANDLE)>, format: u32, handle: HANDLE) {
        if acquired.iter().any(|(existing, _)| *existing == format) {
            // SAFETY: `handle` came from `GlobalAlloc` and has not yet
            // been transferred via `SetClipboardData`.
            unsafe { GlobalFree(handle) };
            return;
        }
        acquired.push((format, handle));
    }

    /// Open the clipboard, empty it, and call `SetClipboardData` for each
    /// `(format, handle)` pair in order. Handles whose `SetClipboardData`
    /// succeeded are owned by the OS; the remaining handles (including
    /// the failing one) are freed before returning the error so a partial
    /// transaction never leaks `HGLOBAL` allocations.
    fn publish_handles(handles: &[(u32, HANDLE)]) -> Result<()> {
        // SAFETY: every Win32 call below is paired with its release.
        // `OpenClipboard(null)` attaches to the calling thread and is
        // unwound by `ClipboardGuard::drop`. Handles that succeed
        // `SetClipboardData` are owned by the OS; handles that fail
        // and the remaining never-transferred handles get explicit
        // `GlobalFree` before returning.
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                for (_, handle) in handles {
                    GlobalFree(*handle);
                }
                return Err(AppError::Platform(
                    "OpenClipboard failed for multi-rep write".to_owned(),
                ));
            }
            let _guard = ClipboardGuard;
            if EmptyClipboard() == 0 {
                for (_, handle) in handles {
                    GlobalFree(*handle);
                }
                return Err(AppError::Platform(
                    "EmptyClipboard failed for multi-rep write".to_owned(),
                ));
            }
            for (index, (format, handle)) in handles.iter().enumerate() {
                if SetClipboardData(*format, *handle).is_null() {
                    // This handle failed and is still ours; every
                    // remaining handle (this one + later) needs freeing.
                    // Earlier handles already transferred ownership to
                    // the OS — do NOT free those.
                    for (_, leftover) in &handles[index..] {
                        GlobalFree(*leftover);
                    }
                    return Err(AppError::Platform(format!(
                        "SetClipboardData(format=0x{format:04x}) failed"
                    )));
                }
            }
            Ok(())
        }
    }

    /// Register a clipboard format by UTF-8 name and return its
    /// session-stable id. Names are encoded to UTF-16 with a NUL
    /// terminator before the call. `RegisterClipboardFormatW` returns
    /// 0 only when the per-session format table is exhausted (49,151
    /// slots), so callers can treat `None` as a non-fatal "no such
    /// row".
    fn register_format(name: &str) -> Option<u32> {
        let mut wide: Vec<u16> = name.encode_utf16().collect();
        wide.push(0);
        // SAFETY: the pointer references a NUL-terminated wide string
        // that lives for the duration of the call.
        let id = unsafe { RegisterClipboardFormatW(wide.as_ptr()) };
        (id != 0).then_some(id)
    }

    /// Allocate a `GMEM_MOVEABLE` `HGLOBAL` and copy `bytes` into it.
    /// Used by every multi-rep payload that ships as a flat byte buffer
    /// (`CF_DIBV5`, the registered "PNG" and "Rich Text Format" rows,
    /// and `CF_HTML` once the wrapper is built).
    fn prepare_byte_buffer(bytes: &[u8]) -> Result<HANDLE> {
        // Win32 `SetClipboardData` is happy with a zero-byte handle, but
        // the empty payload would be a no-op for every consumer; refuse
        // it so the caller catches the case rather than publishing an
        // empty offer.
        if bytes.is_empty() {
            return Err(AppError::Platform(
                "refusing to publish an empty payload".to_owned(),
            ));
        }
        // SAFETY: `GlobalAlloc` returns null on failure (handled below).
        // On success, `GlobalLock` returns a writable pointer to a
        // contiguous region of at least `bytes.len()` bytes — that is
        // the contract `GMEM_MOVEABLE` provides, and `GlobalSize`
        // confirms it. We unlock before returning so the handle is in
        // a publishable state when `SetClipboardData` runs.
        unsafe {
            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
            if handle.is_null() {
                return Err(AppError::Platform(
                    "GlobalAlloc failed for clipboard payload".to_owned(),
                ));
            }
            let locked = GlobalLock(handle);
            if locked.is_null() {
                GlobalFree(handle);
                return Err(AppError::Platform(
                    "GlobalLock failed for clipboard payload".to_owned(),
                ));
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), locked.cast::<u8>(), bytes.len());
            let _ = GlobalUnlock(handle);
            Ok(handle)
        }
    }

    /// Build a NUL-terminated UTF-16 `HGLOBAL` for `CF_UNICODETEXT`.
    fn prepare_cf_unicode_text(text: &str) -> Result<HANDLE> {
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        wide.push(0);
        let byte_len = wide.len().saturating_mul(mem::size_of::<u16>());
        let bytes = unsafe { slice::from_raw_parts(wide.as_ptr().cast::<u8>(), byte_len) };
        prepare_byte_buffer(bytes)
    }

    /// Build a `CF_HTML`-wrapped `HGLOBAL` from an HTML fragment.
    /// The wrapper layout is documented at
    /// <https://learn.microsoft.com/en-us/windows/win32/dataxchg/html-clipboard-format>.
    fn prepare_cf_html(html: &str) -> Result<HANDLE> {
        let wrapped = build_cf_html(html);
        prepare_byte_buffer(wrapped.as_bytes())
    }

    /// Build a `CF_HDROP` `HGLOBAL` containing the given paths.
    pub(super) fn prepare_cf_hdrop(paths: &[String]) -> Result<HANDLE> {
        // Build the wide-char buffer first so we can reject pathological
        // inputs (paths containing interior NULs, lengths above the Win32
        // long-path cap) before touching the clipboard at all.
        let mut wide_buffer: Vec<u16> = Vec::new();
        for path in paths {
            let encoded: Vec<u16> = OsString::from(path).encode_wide().collect();
            if encoded.contains(&0) {
                return Err(AppError::Unsupported(format!(
                    "path {path:?} contains an interior NUL; cannot publish as CF_HDROP",
                )));
            }
            if encoded.len() >= MAX_PATH_WCHARS as usize {
                return Err(AppError::Unsupported(format!(
                    "path {path:?} exceeds the Win32 long-path limit",
                )));
            }
            wide_buffer.extend_from_slice(&encoded);
            wide_buffer.push(0);
        }
        // Terminate the path list with an extra NUL so receivers know
        // where it ends. `DROPFILES.fWide = TRUE` means the terminator
        // is a single 16-bit NUL, which `wide_buffer.push(0)` already
        // appended at the end of the last path; add one more to close
        // the list.
        wide_buffer.push(0);

        let header_size = mem::size_of::<DROPFILES>();
        let header_size_u32 = u32::try_from(header_size)
            .map_err(|_| AppError::Platform("DROPFILES header size exceeds u32".to_owned()))?;
        let payload_bytes = wide_buffer.len().saturating_mul(mem::size_of::<u16>());
        let total_bytes = header_size.saturating_add(payload_bytes);

        // SAFETY: every Win32 call below is paired with its release.
        // The handle is freed on every error path before `SetClipboardData`
        // can claim ownership; callers that successfully publish the
        // handle must NOT free it themselves.
        unsafe {
            let handle = GlobalAlloc(GMEM_MOVEABLE, total_bytes);
            if handle.is_null() {
                return Err(AppError::Platform(
                    "GlobalAlloc failed for CF_HDROP payload".to_owned(),
                ));
            }
            let locked = GlobalLock(handle);
            if locked.is_null() {
                GlobalFree(handle);
                return Err(AppError::Platform(
                    "GlobalLock failed for CF_HDROP payload".to_owned(),
                ));
            }
            let header = DROPFILES {
                pFiles: header_size_u32,
                pt: windows_sys::Win32::Foundation::POINT { x: 0, y: 0 },
                fNC: 0,
                fWide: TRUE,
            };
            std::ptr::copy_nonoverlapping(
                std::ptr::from_ref(&header).cast::<u8>(),
                locked.cast::<u8>(),
                header_size,
            );
            std::ptr::copy_nonoverlapping(
                wide_buffer.as_ptr().cast::<u8>(),
                locked.cast::<u8>().add(header_size),
                payload_bytes,
            );
            let _ = GlobalUnlock(handle);
            Ok(handle)
        }
    }

    /// Compose the `CF_HTML` wrapper for a fragment. The wrapper requires
    /// byte offsets for the `<html>` start, `</html>` end, fragment start,
    /// and fragment end; offsets are 10-digit zero-padded decimals so the
    /// header length is fixed and the placeholders can be replaced in
    /// place once the body length is known.
    ///
    /// Exposed at module scope (instead of inside `unsafe`) so unit
    /// tests on any host can verify the offsets match the bytes they
    /// reference.
    pub(super) fn build_cf_html(fragment: &str) -> String {
        // Build the body first so we can compute byte offsets relative
        // to the start of the wrapper.
        let body_prefix = "<html>\r\n<body>\r\n<!--StartFragment-->";
        let body_suffix = "<!--EndFragment-->\r\n</body>\r\n</html>";
        // 10-digit zero-padded placeholders so substituting actual
        // offsets does not change the header length.
        let header_template = "Version:0.9\r\n\
            StartHTML:0000000000\r\n\
            EndHTML:0000000000\r\n\
            StartFragment:0000000000\r\n\
            EndFragment:0000000000\r\n";
        let header_len = header_template.len();
        let start_html = header_len;
        let start_fragment = start_html + body_prefix.len();
        let end_fragment = start_fragment + fragment.len();
        let end_html = end_fragment + body_suffix.len();

        // Format real offsets. The header was sized to fit 10 digits;
        // payloads larger than ~9.9 GB cannot be expressed and would
        // exceed Win32 clipboard limits anyway, so we accept the
        // bound implicitly.
        let header = format!(
            "Version:0.9\r\n\
            StartHTML:{start_html:010}\r\n\
            EndHTML:{end_html:010}\r\n\
            StartFragment:{start_fragment:010}\r\n\
            EndFragment:{end_fragment:010}\r\n"
        );
        debug_assert_eq!(
            header.len(),
            header_len,
            "CF_HTML header changed size after offset substitution",
        );

        let mut out = String::with_capacity(
            header.len() + body_prefix.len() + fragment.len() + body_suffix.len(),
        );
        out.push_str(&header);
        out.push_str(body_prefix);
        out.push_str(fragment);
        out.push_str(body_suffix);
        out
    }

    /// Decode an encoded image (PNG/JPEG/GIF/WebP/TIFF) and emit a
    /// `CF_DIBV5` byte buffer with the canonical Word-compatible layout:
    /// 124-byte `BITMAPV5HEADER`, `BI_BITFIELDS` compression, BGRA
    /// channel order, bottom-up rows (positive height), and the sRGB
    /// colour space.
    ///
    /// Exposed at module scope (instead of inside `unsafe`) so unit
    /// tests on any host can verify the header layout and pixel-byte
    /// order match expectations.
    pub(super) fn build_dibv5_payload(encoded: &[u8]) -> Result<Vec<u8>> {
        // Multi-rep copy-back walks every stored representation through
        // `build_dibv5_payload`, so the same encoded-vs-decoded asymmetry
        // applies here as in the single-image path. Probe dimensions before
        // the `decode` call materialises the RGBA buffer.
        super::reject_oversized_image(encoded)?;
        let rgba = ImageReader::new(Cursor::new(encoded))
            .with_guessed_format()
            .map_err(|err| AppError::Platform(format!("image probe failed: {err}")))?
            .decode()
            .map_err(|err| AppError::Platform(format!("image decode failed: {err}")))?
            .to_rgba8();
        let (width, height) = rgba.dimensions();
        if width == 0 || height == 0 {
            return Err(AppError::Platform(
                "image has zero width or height; cannot publish as CF_DIBV5".to_owned(),
            ));
        }
        let width_i32 = i32::try_from(width)
            .map_err(|_| AppError::Platform("image width exceeds i32".to_owned()))?;
        let height_i32 = i32::try_from(height)
            .map_err(|_| AppError::Platform("image height exceeds i32".to_owned()))?;
        let stride = (width as usize).saturating_mul(4);
        let size_image = stride
            .checked_mul(height as usize)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| {
                AppError::Platform("image dimensions overflow CF_DIBV5 size field".to_owned())
            })?;

        // SAFETY: `BITMAPV5HEADER` is a `repr(C)` struct made entirely
        // of integers plus a `CIEXYZTRIPLE` of integers; the all-zero
        // representation is valid and we overwrite every field we care
        // about below.
        let mut header: BITMAPV5HEADER = unsafe { mem::zeroed() };
        // `BITMAPV5HEADER` is 124 bytes by spec; `try_from` keeps the
        // conversion explicit instead of relying on `as u32` and a
        // matching clippy allow.
        header.bV5Size = u32::try_from(mem::size_of::<BITMAPV5HEADER>())
            .map_err(|_| AppError::Platform("BITMAPV5HEADER size exceeds u32".to_owned()))?;
        header.bV5Width = width_i32;
        // POSITIVE height = bottom-up scan order, the layout MS Word /
        // Paint pull from `CF_DIBV5`. Top-down (negative height) is
        // valid by spec but Word renders it upside-down.
        header.bV5Height = height_i32;
        header.bV5Planes = 1;
        header.bV5BitCount = 32;
        header.bV5Compression = BI_BITFIELDS;
        header.bV5SizeImage = size_image;
        header.bV5RedMask = 0x00FF_0000;
        header.bV5GreenMask = 0x0000_FF00;
        header.bV5BlueMask = 0x0000_00FF;
        header.bV5AlphaMask = 0xFF00_0000;
        // 'sRGB' little-endian == 0x73524742, the documented value for
        // an sRGB colour space. Hand-coded so the constant stays
        // visible at the call site (windows-sys exposes it under the
        // `LCS_sRGB` symbol on some feature bundles but not all).
        header.bV5CSType = 0x7352_4742;
        header.bV5Intent = LCS_GM_IMAGES as u32;

        let header_size = mem::size_of::<BITMAPV5HEADER>();
        let mut out =
            Vec::with_capacity(header_size.saturating_add(stride.saturating_mul(height as usize)));
        // SAFETY: `header` is a fully-initialised `BITMAPV5HEADER` and
        // we read its bytes through a raw pointer — valid because the
        // type is `repr(C)`.
        out.extend_from_slice(unsafe {
            slice::from_raw_parts(std::ptr::from_ref(&header).cast::<u8>(), header_size)
        });

        // RGBA → BGRA + vertical flip. Iterate rows from the last row
        // upward so the first row written to the buffer is the bottom
        // row of the image (required by positive-height DIB layout).
        let raw = rgba.as_raw();
        for row in (0..height as usize).rev() {
            let start = row.saturating_mul(stride);
            let end = start.saturating_add(stride);
            for pixel in raw[start..end].chunks_exact(4) {
                out.push(pixel[2]); // B
                out.push(pixel[1]); // G
                out.push(pixel[0]); // R
                out.push(pixel[3]); // A
            }
        }
        Ok(out)
    }
}

fn platform_err(err: &arboard::Error) -> AppError {
    AppError::Platform(err.to_string())
}

fn lock_err<T>(err: &std::sync::PoisonError<T>) -> AppError {
    AppError::Platform(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagori_core::RepresentationRole;

    #[test]
    fn has_publishable_representation_matches_known_mimes() {
        let plain = StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "text/plain".to_owned(),
            ordinal: 0,
            data: RepresentationDataRef::InlineText("hi".to_owned()),
        };
        let html = StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "text/html".to_owned(),
            ordinal: 1,
            data: RepresentationDataRef::InlineText("<p>hi</p>".to_owned()),
        };
        let png = StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "image/png".to_owned(),
            ordinal: 2,
            data: RepresentationDataRef::DatabaseBlob(vec![0x89, 0x50, 0x4e, 0x47]),
        };
        let paths = StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "text/uri-list".to_owned(),
            ordinal: 3,
            data: RepresentationDataRef::FilePaths(vec!["C:\\one".to_owned()]),
        };
        assert!(has_publishable_representation(&[plain]));
        assert!(has_publishable_representation(&[html]));
        assert!(has_publishable_representation(&[png]));
        assert!(has_publishable_representation(&[paths]));
    }

    /// CRC-32 (PNG/IEEE 802.3 polynomial 0xEDB88320).
    ///
    /// Hand-rolled instead of pulling in `crc32fast` so the dev-dep set
    /// stays untouched. PNG's IHDR chunk fails to parse without a valid
    /// CRC, so the forged-header test below needs to compute one.
    fn png_crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFF_u32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }

    /// Build a PNG header with arbitrary advertised dimensions.
    ///
    /// Emits the 8-byte signature, a valid IHDR chunk advertising
    /// `width × height`, an IDAT chunk holding a zero-byte zlib stream,
    /// and IEND. `image::ImageReader::into_dimensions` parses the chunk
    /// stream until it finds IDAT before exposing dimensions, so the
    /// IDAT marker is mandatory even though we never invoke `decode`.
    /// The encoded payload stays under ~100 bytes regardless of the
    /// dimensions encoded, which is the whole point of the fixture:
    /// proving that a tiny encoded blob can advertise a multi-GB canvas.
    /// Append a single PNG chunk to `out` with the canonical
    /// `length + type + payload + CRC` layout. Shared between
    /// `forge_png_header` and the ancillary-chunk regression test.
    fn push_chunk_for_test(out: &mut Vec<u8>, chunk_type: [u8; 4], payload: &[u8]) {
        let length = u32::try_from(payload.len()).expect("chunk payload fits in u32");
        out.extend_from_slice(&length.to_be_bytes());
        let mut typed_payload = Vec::with_capacity(4 + payload.len());
        typed_payload.extend_from_slice(&chunk_type);
        typed_payload.extend_from_slice(payload);
        let crc = png_crc32(&typed_payload);
        out.extend_from_slice(&typed_payload);
        out.extend_from_slice(&crc.to_be_bytes());
    }

    fn forge_png_header(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::new();
        // PNG signature.
        out.extend_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
        // IHDR: 13-byte payload — width, height, depth, colour type,
        // compression, filter, interlace.
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.push(8); // bit depth
        ihdr.push(2); // colour type (RGB)
        ihdr.push(0); // compression
        ihdr.push(0); // filter
        ihdr.push(0); // interlace
        push_chunk_for_test(&mut out, *b"IHDR", &ihdr);
        // Minimal zlib empty stream (`78 9C 03 00 00 00 00 01`).
        push_chunk_for_test(
            &mut out,
            *b"IDAT",
            &[0x78, 0x9C, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01],
        );
        push_chunk_for_test(&mut out, *b"IEND", &[]);
        out
    }

    fn encode_real_png(width: u32, height: u32) -> Vec<u8> {
        let mut png = Vec::new();
        let img = image::RgbaImage::new(width, height);
        img.write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
            .expect("encode small PNG");
        png
    }

    #[test]
    fn image_pixel_count_from_encoded_reads_forged_png_header() {
        // Bare IHDR — no IDAT / IEND, decode would fail, but the
        // dimensions probe must still report the advertised canvas so
        // copy-back can reject pre-decode.
        let forged = forge_png_header(40_000, 40_000);
        let pixels = image_pixel_count_from_encoded(&forged).expect("dimensions parse");
        assert_eq!(pixels, 40_000_u64 * 40_000_u64);
    }

    #[test]
    fn image_pixel_count_from_encoded_returns_none_for_non_image_bytes() {
        // No format guess possible → caller falls through to whatever
        // platform-level error the downstream `decode` produces.
        assert!(image_pixel_count_from_encoded(b"definitely not an image").is_none());
        assert!(image_pixel_count_from_encoded(&[]).is_none());
    }

    #[test]
    fn png_pixel_count_from_ihdr_reads_real_and_forged_headers() {
        // The capture probe runs against a 32-byte prefix copied out of
        // the clipboard `HGLOBAL`, so it must work without seeing IDAT
        // (which `image::ImageReader::into_dimensions` requires). The
        // forged header has only IHDR + IDAT + IEND; the prefix path
        // must agree with the encoded full PNG on dimensions.
        let forged = forge_png_header(40_000, 40_000);
        assert_eq!(
            png_pixel_count_from_ihdr(&forged[..24]),
            Some(40_000_u64 * 40_000_u64),
        );
        let real = encode_real_png(8, 8);
        assert_eq!(png_pixel_count_from_ihdr(&real[..24]), Some(64));
    }

    #[test]
    fn png_pixel_count_from_ihdr_rejects_wrong_signature_or_chunk() {
        // Truncated prefix → None so the daemon falls through to the
        // regular capture path and any downstream decode error surfaces.
        assert!(png_pixel_count_from_ihdr(&[0u8; 23]).is_none());
        // Wrong signature.
        assert!(png_pixel_count_from_ihdr(&[0u8; 24]).is_none());
        // Right signature but wrong first chunk type.
        let mut bogus = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        bogus.extend_from_slice(&13_u32.to_be_bytes());
        bogus.extend_from_slice(b"WRNG");
        bogus.extend_from_slice(&[0u8; 8]);
        assert!(png_pixel_count_from_ihdr(&bogus).is_none());
    }

    #[test]
    fn png_pixel_count_from_ihdr_survives_real_png_with_ancillary_chunks() {
        // The regression codex flagged: a valid PNG with `gAMA` / `sRGB`
        // / `pHYs` between IHDR and IDAT would push IDAT past a 32-byte
        // peek, making `into_dimensions` return None. The IHDR-only
        // parser must still recover the dimensions from the first 24
        // bytes regardless of what follows.
        let mut png = Vec::new();
        png.extend_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
        push_chunk_for_test(&mut png, *b"IHDR", &{
            let mut ihdr = Vec::with_capacity(13);
            ihdr.extend_from_slice(&65_535_u32.to_be_bytes());
            ihdr.extend_from_slice(&65_535_u32.to_be_bytes());
            ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
            ihdr
        });
        // Ancillary chunks deliberately injected before IDAT.
        push_chunk_for_test(&mut png, *b"gAMA", &[0x00, 0x00, 0xB1, 0x8F]);
        push_chunk_for_test(&mut png, *b"sRGB", &[0]);
        push_chunk_for_test(
            &mut png,
            *b"pHYs",
            &[0x00, 0x00, 0x0B, 0x13, 0x00, 0x00, 0x0B, 0x13, 0x01],
        );
        push_chunk_for_test(
            &mut png,
            *b"IDAT",
            &[0x78, 0x9C, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01],
        );
        push_chunk_for_test(&mut png, *b"IEND", &[]);
        assert_eq!(
            png_pixel_count_from_ihdr(&png[..24]),
            Some(65_535_u64 * 65_535_u64),
        );
    }

    /// Smallest square dimension whose product exceeds
    /// `MAX_DECODED_IMAGE_PIXELS`, derived via integer sqrt to avoid f64
    /// cast lints. Centralised so both the cross-platform and
    /// Windows-only rejection tests stay in sync as the cap evolves.
    fn dim_above_cap() -> u32 {
        let cap = MAX_DECODED_IMAGE_PIXELS;
        let dim = u32::try_from(cap.isqrt()).expect("isqrt fits in u32") + 1;
        assert!(u64::from(dim).saturating_mul(u64::from(dim)) > cap);
        dim
    }

    #[test]
    fn reject_oversized_image_blocks_canvas_above_cap() {
        // Forged PNG that advertises a pixel count above
        // MAX_DECODED_IMAGE_PIXELS but encodes to a few-dozen bytes —
        // exactly the asymmetric payload a decompression-bomb guard must
        // reject. Must return Unsupported so `write_image_bytes` /
        // `build_dibv5_payload` bail before `decode()` allocates the
        // multi-GB RGBA buffer.
        let dim = dim_above_cap();
        let forged = forge_png_header(dim, dim);
        let err = reject_oversized_image(&forged).expect_err("must reject above cap");
        assert!(matches!(err, AppError::Unsupported(_)), "got {err:?}");
    }

    #[test]
    fn reject_oversized_image_allows_canvas_at_or_below_cap() {
        // A real small PNG must pass without complaint so legitimate
        // captures keep round-tripping through the copy-back path.
        let small = encode_real_png(8, 8);
        reject_oversized_image(&small).expect("small PNG must be accepted");
    }

    #[test]
    fn reject_oversized_image_tolerates_unrecognised_bytes() {
        // Probe failure (unknown format) is not the cap's job to report;
        // the downstream `decode` produces a clearer platform error.
        reject_oversized_image(b"not an image at all").expect("unknown bytes pass the cap");
    }

    #[test]
    fn dib_pixel_count_from_header_reads_top_down_and_bottom_up() {
        // bV5Size = 124, biWidth = 10, biHeight = +5 (bottom-up).
        let mut bottom_up = vec![0u8; 12];
        bottom_up[0..4].copy_from_slice(&124_u32.to_le_bytes());
        bottom_up[4..8].copy_from_slice(&10_i32.to_le_bytes());
        bottom_up[8..12].copy_from_slice(&5_i32.to_le_bytes());
        assert_eq!(dib_pixel_count_from_header(&bottom_up), Some(50));

        // Negative biHeight (top-down DIB) — pixel count uses the
        // absolute value of both axes.
        let mut top_down = bottom_up.clone();
        top_down[8..12].copy_from_slice(&(-5_i32).to_le_bytes());
        assert_eq!(dib_pixel_count_from_header(&top_down), Some(50));

        // Short prefix → None.
        assert_eq!(dib_pixel_count_from_header(&bottom_up[..8]), None);
    }

    #[test]
    fn dib_pixel_count_from_header_flags_pathological_dimensions() {
        // i32::MIN.unsigned_abs() == 2_147_483_648 — exercising the
        // unsigned_abs path so a top-down DIB with the maximum-magnitude
        // height still yields a finite pixel count via saturating_mul.
        let mut header = vec![0u8; 12];
        header[0..4].copy_from_slice(&40_u32.to_le_bytes());
        header[4..8].copy_from_slice(&1_i32.to_le_bytes());
        header[8..12].copy_from_slice(&i32::MIN.to_le_bytes());
        assert_eq!(dib_pixel_count_from_header(&header), Some(1 << 31));
    }

    #[cfg(windows)]
    #[test]
    fn build_dibv5_payload_rejects_canvas_above_cap() {
        // Same forged PNG fixture as the top-level reject test; the
        // multi-rep copy-back path runs through `build_dibv5_payload`
        // on Windows hosts and must bail before decode allocates.
        let dim = dim_above_cap();
        let forged = forge_png_header(dim, dim);
        let err = win::build_dibv5_payload(&forged).expect_err("must reject above cap");
        assert!(matches!(err, AppError::Unsupported(_)), "got {err:?}");
    }

    #[test]
    fn has_publishable_representation_rejects_unmapped_mimes() {
        let json = StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "application/json".to_owned(),
            ordinal: 0,
            data: RepresentationDataRef::InlineText("{}".to_owned()),
        };
        let empty_paths = StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "text/uri-list".to_owned(),
            ordinal: 1,
            data: RepresentationDataRef::FilePaths(Vec::new()),
        };
        assert!(!has_publishable_representation(&[]));
        assert!(!has_publishable_representation(&[json, empty_paths]));
    }

    #[cfg(windows)]
    #[test]
    fn build_cf_html_wrapper_offsets_are_consistent() {
        let fragment = "<p>hello <b>world</b></p>";
        let wrapped = win::build_cf_html(fragment);
        let bytes = wrapped.as_bytes();

        let find_value = |key: &str| -> usize {
            let needle = format!("{key}:");
            let start = wrapped.find(&needle).expect("header line present") + needle.len();
            let end = wrapped[start..]
                .find("\r\n")
                .expect("header line is CRLF terminated")
                + start;
            wrapped[start..end]
                .parse::<usize>()
                .expect("offset is a decimal integer")
        };
        let start_html = find_value("StartHTML");
        let end_html = find_value("EndHTML");
        let start_fragment = find_value("StartFragment");
        let end_fragment = find_value("EndFragment");

        // <html> must start exactly at StartHTML.
        assert_eq!(&bytes[start_html..start_html + 6], b"<html>");
        // Fragment substring lives between StartFragment and EndFragment.
        assert_eq!(&bytes[start_fragment..end_fragment], fragment.as_bytes());
        // </html> must close right before EndHTML.
        assert_eq!(&bytes[end_html - 7..end_html], b"</html>");
        // Header length is fixed-width (offsets are zero-padded to 10
        // digits), so StartHTML equals the header line count × 64.
        assert!(start_html >= b"Version:0.9\r\n".len());
    }

    #[cfg(windows)]
    #[test]
    fn build_dibv5_payload_round_trips_1x1_png() {
        // 1×1 RGBA(0xAA, 0xBB, 0xCC, 0xDD) PNG, generated via the image
        // crate so the test does not bake an opaque base64 blob.
        let mut png = Vec::new();
        let pixel = image::Rgba::<u8>([0xAA, 0xBB, 0xCC, 0xDD]);
        let mut img = image::RgbaImage::new(1, 1);
        img.put_pixel(0, 0, pixel);
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("encode 1x1 PNG");

        let payload = win::build_dibv5_payload(&png).expect("DIBV5 builds");

        // 124-byte BITMAPV5HEADER + 4 pixel bytes.
        assert_eq!(payload.len(), 124 + 4);
        // bV5Size at offset 0 = 124.
        assert_eq!(
            u32::from_le_bytes(payload[0..4].try_into().unwrap()),
            124,
            "bV5Size",
        );
        // bV5Width at offset 4 = 1.
        assert_eq!(
            i32::from_le_bytes(payload[4..8].try_into().unwrap()),
            1,
            "bV5Width",
        );
        // bV5Height at offset 8 = 1 (POSITIVE = bottom-up).
        assert_eq!(
            i32::from_le_bytes(payload[8..12].try_into().unwrap()),
            1,
            "bV5Height (positive ⇒ bottom-up rows for Word compat)",
        );
        // bV5BitCount at offset 14 = 32.
        assert_eq!(
            u16::from_le_bytes(payload[14..16].try_into().unwrap()),
            32,
            "bV5BitCount",
        );
        // bV5Compression at offset 16 = BI_BITFIELDS (3).
        assert_eq!(
            u32::from_le_bytes(payload[16..20].try_into().unwrap()),
            3,
            "bV5Compression == BI_BITFIELDS",
        );
        // bV5CSType at offset 56 = 'sRGB' = 0x73524742.
        assert_eq!(
            u32::from_le_bytes(payload[56..60].try_into().unwrap()),
            0x7352_4742,
            "bV5CSType == LCS_sRGB",
        );

        // Pixel bytes: input RGBA(0xAA,0xBB,0xCC,0xDD) → output
        // BGRA(0xCC,0xBB,0xAA,0xDD).
        assert_eq!(&payload[124..128], &[0xCC, 0xBB, 0xAA, 0xDD]);
    }

    fn snapshot_with(reps: Vec<ClipboardRepresentation>) -> ClipboardSnapshot {
        ClipboardSnapshot {
            sequence: ClipboardSequence::native(7),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: reps,
        }
    }

    #[test]
    fn total_payload_bytes_sums_text_image_and_file_paths() {
        // The byte-budget check in `capture_snapshot` leans on this sum
        // counting every representation: UTF-8 text length, raw image
        // bytes, and the concatenated path lengths.
        let snapshot = snapshot_with(vec![
            ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text("hello".to_owned()), // 5
            },
            ClipboardRepresentation {
                mime_type: "image/png".to_owned(),
                data: ClipboardData::Bytes(vec![0u8; 10]), // 10
            },
            ClipboardRepresentation {
                mime_type: "text/uri-list".to_owned(),
                data: ClipboardData::FilePaths(vec!["abc".to_owned(), "de".to_owned()]), // 3 + 2
            },
        ]);
        assert_eq!(total_payload_bytes(&snapshot), 5 + 10 + 3 + 2);
    }

    #[test]
    fn total_payload_bytes_is_zero_for_an_empty_snapshot() {
        assert_eq!(total_payload_bytes(&snapshot_with(Vec::new())), 0);
    }

    #[test]
    fn encode_rgba_to_png_round_trips_a_small_image() {
        // 2×1 RGBA: red then green, both opaque. The capture path hands
        // `encode_rgba_to_png` arboard's raw RGBA and expects a decodable
        // PNG that preserves dimensions and pixel order.
        let raw = vec![
            0xFF, 0x00, 0x00, 0xFF, // red
            0x00, 0xFF, 0x00, 0xFF, // green
        ];
        let png = encode_rgba_to_png(ImageData {
            width: 2,
            height: 1,
            bytes: Cow::Owned(raw),
        })
        .expect("encode succeeds for a well-formed RGBA buffer");

        let decoded = ImageReader::new(Cursor::new(&png))
            .with_guessed_format()
            .expect("guess format")
            .decode()
            .expect("decode PNG")
            .to_rgba8();
        assert_eq!(decoded.dimensions(), (2, 1));
        assert_eq!(decoded.get_pixel(0, 0).0, [0xFF, 0x00, 0x00, 0xFF]);
        assert_eq!(decoded.get_pixel(1, 0).0, [0x00, 0xFF, 0x00, 0xFF]);
    }

    #[test]
    fn encode_rgba_to_png_rejects_a_buffer_that_underfills_its_dimensions() {
        // `RgbaImage::from_raw` returns None when the buffer is too small
        // for width × height × 4, so the encoder must surface None rather
        // than publish a torn image.
        let png = encode_rgba_to_png(ImageData {
            width: 4,
            height: 4,
            bytes: Cow::Owned(vec![0u8; 4]), // needs 64 bytes
        });
        assert!(png.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn build_cf_html_offsets_are_byte_based_for_multibyte_fragments() {
        // CF_HTML header offsets are *byte* offsets, so a multi-byte UTF-8
        // fragment must still satisfy EndFragment - StartFragment ==
        // fragment.len() (bytes). A char-count regression would misplace
        // the offsets Word / Outlook use to locate the fragment.
        let fragment = "café 日本語 <b>x</b>";
        assert!(
            fragment.len() > fragment.chars().count(),
            "fixture must contain multi-byte characters",
        );
        let wrapped = win::build_cf_html(fragment);

        let find_value = |key: &str| -> usize {
            let needle = format!("{key}:");
            let start = wrapped.find(&needle).expect("header line present") + needle.len();
            let end = wrapped[start..].find("\r\n").expect("CRLF terminated") + start;
            wrapped[start..end]
                .parse::<usize>()
                .expect("decimal offset")
        };
        let start_fragment = find_value("StartFragment");
        let end_fragment = find_value("EndFragment");
        assert_eq!(end_fragment - start_fragment, fragment.len());
        assert_eq!(
            &wrapped.as_bytes()[start_fragment..end_fragment],
            fragment.as_bytes(),
        );
    }

    #[cfg(windows)]
    #[test]
    fn prepare_cf_hdrop_rejects_interior_nul_and_overlong_paths() {
        // Both rejections fire before any Win32 allocation, so they are
        // safe to exercise without a real clipboard. An interior NUL would
        // truncate the wide path mid-string; an over-long path exceeds the
        // Win32 long-path cap and signals a corrupt / hostile DROPFILES.
        let interior_nul = win::prepare_cf_hdrop(&["a\u{0}b".to_owned()]);
        assert!(matches!(interior_nul, Err(AppError::Unsupported(_))));

        let overlong = format!("C:\\{}", "a".repeat(33_000));
        let too_long = win::prepare_cf_hdrop(&[overlong]);
        assert!(matches!(too_long, Err(AppError::Unsupported(_))));
    }
}
