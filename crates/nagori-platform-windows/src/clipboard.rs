use std::borrow::Cow;
use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arboard::{Clipboard, ImageData};
use async_trait::async_trait;
use image::ImageFormat;
use nagori_core::{
    AppError, ClipboardContent, ClipboardData, ClipboardEntry, ClipboardRepresentation,
    ClipboardSequence, ClipboardSnapshot, MAX_DECODED_IMAGE_PIXELS, ReadBudget, Result,
    StoredClipboardRepresentation,
};
use nagori_platform::{
    CapturedSnapshot, ClipboardReader, ClipboardWriter, SNAPSHOT_CAPTURE_MAX_RETRIES,
    clipboard_blocking, clipboard_write_blocking, decode_rgba_with_pixel_cap,
    has_publishable_representation, lock_clipboard_for_write, lock_clipboard_recovering,
    platform_err,
};
use time::OffsetDateTime;

/// Hard ceiling on a single clipboard *text* representation copied into the
/// daemon's heap on the unbounded `current_snapshot` path.
///
/// The bounded `current_snapshot_with_max` path already rejects oversized text
/// via `win::oversized_payload` before `get_text`; this guards the budget-less
/// path so a hostile clipboard owner cannot land a multi-GB string in our
/// address space. Mirrors the macOS adapter's `MAX_TEXT_REP_BYTES` and the
/// Linux adapter's `INTERNAL_BODY_CEILING_BYTES` — 256 MiB is far above any
/// realistic copied document.
const MAX_TEXT_REP_BYTES: usize = 256 * 1024 * 1024;

/// Brief pause between torn-snapshot retries.
///
/// Three immediate retries during a foreign write storm burn out in a
/// sub-millisecond window and all observe the same torn state. A short sleep
/// (the read runs on the blocking pool, so sleeping is fine) lets the foreign
/// writer settle, raising the odds the next attempt reads a stable sequence
/// number. Mirrors the macOS adapter's `TORN_RETRY_BACKOFF`.
const TORN_RETRY_BACKOFF: Duration = Duration::from_millis(1);

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
        let (captured, image) = clipboard_blocking("current_snapshot", move || {
            capture_snapshot(&clipboard, None)
        })
        .await
        .map_err(|err| AppError::Platform(err.to_string()))??;
        // Encode any captured image to PNG off the read timeout (see
        // `finalize_capture`); the raw bytes are already captured under the
        // clipboard lock above.
        match finalize_capture(captured, image, None).await? {
            CapturedSnapshot::Captured(snapshot) => Ok(snapshot),
            CapturedSnapshot::Oversized { .. } => unreachable!("unbounded capture cannot skip"),
            // The unbounded path has no `CapturedSnapshot` to return, so an
            // owner-excluded clip yields an empty snapshot — we never
            // materialise the secret body (mirroring the macOS adapter).
            CapturedSnapshot::Excluded { sequence, .. } => Ok(ClipboardSnapshot {
                sequence,
                captured_at: OffsetDateTime::now_utc(),
                source: None,
                representations: Vec::new(),
            }),
        }
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

    async fn current_snapshot_with_max(&self, budget: ReadBudget) -> Result<CapturedSnapshot> {
        let clipboard = self.clipboard.clone();
        let (captured, image) = clipboard_blocking("current_snapshot_with_max", move || {
            capture_snapshot(&clipboard, Some(budget))
        })
        .await
        .map_err(|err| AppError::Platform(err.to_string()))??;
        // Encode any captured image to PNG off the read timeout, then apply
        // the per-kind budget to the full payload (see `finalize_capture`).
        finalize_capture(captured, image, Some(budget)).await
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
                // Acquisition is bounded so a guard leaked by a timed-out
                // read cannot park the write.
                let _guard = lock_clipboard_for_write(&clipboard, "write_representations")?;
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
                let _guard = lock_clipboard_for_write(&clipboard, "write_representation_exact")?;
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
            // offer) and `SetClipboardData`. Acquisition is bounded so a
            // guard leaked by a timed-out read cannot park the write.
            let _guard = lock_clipboard_for_write(&clipboard, "write_files")?;
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
            // The shared helper probes dimensions first so an encoded
            // payload whose advertised canvas blows past
            // `MAX_DECODED_IMAGE_PIXELS` (e.g. a 1 KB PNG claiming
            // 65535×65535) is rejected before `decode` allocates a multi-GB
            // RGBA buffer.
            let rgba = decode_rgba_with_pixel_cap(&bytes, MAX_DECODED_IMAGE_PIXELS)
                .map_err(|err| decode_err_to_app_error(&err))?;
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
            // Bounded lock acquisition; the `set_image` itself still runs
            // to completion unbounded (see the decode rationale above).
            lock_clipboard_for_write(&clipboard, "write_image_bytes")?
                .set_image(image_data)
                .map_err(|err| platform_err(&err))
        })
        .await
        .map_err(|err| AppError::Platform(err.to_string()))?
    }
}

/// Map a shared decode rejection onto this adapter's error split: the
/// decompression-bomb cap is `Unsupported` (the rejection is reported
/// upward as a policy refusal), everything else is a `Platform` failure.
fn decode_err_to_app_error(err: &nagori_platform::DecodeRgbaError) -> AppError {
    match err {
        nagori_platform::DecodeRgbaError::PixelCapExceeded { .. } => {
            AppError::Unsupported(err.to_string())
        }
        _ => AppError::Platform(err.to_string()),
    }
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

/// One bounded clipboard read, deferring the CPU-bound image encode.
///
/// Returns the captured snapshot together with any *raw* image payload
/// (`CF_DIBV5` / registered `"PNG"` bytes, copied out under the clipboard
/// lock) and the index it should occupy in the representation order. The
/// CPU-bound decode (DIB/PNG -> RGBA) and PNG re-encode are **not** done
/// here: they run outside [`CLIPBOARD_OP_TIMEOUT`] in [`finalize_capture`].
/// Copying the raw bytes is a bounded memcpy (`image_pixel_overflow` already
/// rejected pathological dimensions, so the uncompressed DIB is at most
/// `MAX_DECODED_IMAGE_PIXELS × 4` bytes); the slow width×height RGBA
/// expansion and the deflate-heavy PNG encode are what made a large-but-valid
/// screenshot trip the 3s read timeout permanently and pinned the detached
/// thread's mutex against later writes.
/// Outcome of the per-attempt owner-exclusion probe in [`capture_snapshot`].
#[cfg(windows)]
enum OwnerExclusionProbe {
    /// No marker present — proceed to the body read.
    Absent,
    /// Marker present and the sequence settled (stable or final retry):
    /// return this `Excluded` snapshot.
    Settled(CapturedSnapshot),
    /// Marker present but the sequence drifted with retries left — discard
    /// and re-read rather than anchoring a torn observation.
    Retry,
}

/// Probe for an owner exclusion marker at one point of a capture attempt
/// (used both before and after the body read).
///
/// Applies the same settle-or-retry decision the oversized path uses: the
/// presence probe runs in its own clipboard session, so a marker found on a
/// sequence that drifted from `before` is retried (up to
/// [`SNAPSHOT_CAPTURE_MAX_RETRIES`]) with the final attempt accepted. An
/// `Unavailable` probe (the clipboard was momentarily locked, e.g. while an
/// owner publishes a marked secret) is inconclusive, so it too retries rather
/// than falling through to a body read; only once retries are exhausted does it
/// give up and let the read proceed. The marker is detected without reading any
/// handle's bytes, so a marked secret never enters our address space.
#[cfg(windows)]
fn owner_exclusion_probe(before: u32, attempt: usize) -> OwnerExclusionProbe {
    match win::owner_exclusion() {
        win::MarkerProbe::Present(kind) => {
            let after = native_sequence_number();
            if before == after || attempt >= SNAPSHOT_CAPTURE_MAX_RETRIES {
                OwnerExclusionProbe::Settled(CapturedSnapshot::Excluded {
                    sequence: ClipboardSequence::native(i64::from(after)),
                    kind,
                })
            } else {
                OwnerExclusionProbe::Retry
            }
        }
        win::MarkerProbe::Unavailable if attempt < SNAPSHOT_CAPTURE_MAX_RETRIES => {
            OwnerExclusionProbe::Retry
        }
        win::MarkerProbe::Unavailable | win::MarkerProbe::Absent => OwnerExclusionProbe::Absent,
    }
}

/// Build the file-list and text representations for a capture attempt and
/// return them together with the index the image rep should occupy (after any
/// file list, before text — the order `capture_snapshot` documents).
///
/// Split out of [`capture_snapshot`] so the retry loop stays readable; the
/// `CF_HDROP` read is Windows-only, so the non-Windows build assembles just the
/// text rep (the daemon never runs there, but the crate still compiles).
fn assemble_text_and_files(plain: Option<String>) -> (Vec<ClipboardRepresentation>, usize) {
    let mut representations = Vec::new();
    #[cfg(windows)]
    if let Some(files) = win::read_file_list() {
        representations.push(ClipboardRepresentation {
            mime_type: "text/uri-list".to_owned(),
            data: ClipboardData::FilePaths(files),
        });
    }
    let image_index = representations.len();
    if let Some(text) = plain {
        representations.push(ClipboardRepresentation {
            mime_type: "text/plain".to_owned(),
            data: ClipboardData::Text(text),
        });
    }
    (representations, image_index)
}

fn capture_snapshot(
    clipboard: &Mutex<Clipboard>,
    budget: Option<ReadBudget>,
) -> Result<(CapturedSnapshot, Option<(usize, RawImage)>)> {
    const MAX_RETRIES: usize = SNAPSHOT_CAPTURE_MAX_RETRIES;
    let mut attempt = 0;
    loop {
        attempt += 1;
        if attempt > 1 {
            // Back off briefly before re-reading so a foreign write storm
            // doesn't consume every retry in the same instant; runs on the
            // blocking pool, so sleeping is fine.
            std::thread::sleep(TORN_RETRY_BACKOFF);
        }
        let before = native_sequence_number();
        // Owner-declared exclusion marker takes precedence over everything
        // else, exactly like the macOS adapter: a marked secret is skipped
        // before `get_text` reads it, so the body never enters our address
        // space. `owner_exclusion_probe` owns the settle/retry handling.
        #[cfg(windows)]
        match owner_exclusion_probe(before, attempt) {
            OwnerExclusionProbe::Settled(snapshot) => return Ok((snapshot, None)),
            OwnerExclusionProbe::Retry => continue,
            OwnerExclusionProbe::Absent => {}
        }
        if let Some(budget) = budget {
            #[cfg(windows)]
            if let Some((observed, limit)) = win::oversized_payload(budget) {
                // The size probe is a separate clipboard session from the
                // `before` sample, so a write landing in between would anchor
                // `last_sequence` to a *different* clip we never sized — and
                // the capture loop, which dedupes on sequence equality, would
                // skip that clip forever. Mirror the settled path below:
                // accept on a stable sequence or the final retry, otherwise
                // discard and retry rather than committing a torn observation.
                let after = native_sequence_number();
                if before == after || attempt >= MAX_RETRIES {
                    return Ok((
                        CapturedSnapshot::Oversized {
                            sequence: ClipboardSequence::native(i64::from(after)),
                            observed_bytes: observed,
                            limit,
                        },
                        None,
                    ));
                }
                continue;
            }
            #[cfg(not(windows))]
            let _ = budget;
        }

        let mut guard = lock_clipboard_recovering(clipboard);
        // Defence-in-depth text ceiling for the unbounded path (the bounded
        // path is already covered by `win::oversized_payload` above). Probe the
        // `CF_UNICODETEXT` byte length before `get_text` copies it so a hostile
        // multi-GB string never lands in our heap.
        #[cfg(windows)]
        let skip_oversized_text =
            budget.is_none() && win::unicode_text_over_ceiling(MAX_TEXT_REP_BYTES);
        #[cfg(not(windows))]
        let skip_oversized_text = false;
        let plain = if skip_oversized_text {
            tracing::warn!(
                ceiling = MAX_TEXT_REP_BYTES,
                "clipboard_text_rep_exceeds_ceiling"
            );
            None
        } else {
            match guard.get_text() {
                Ok(text) => Some(text),
                Err(arboard::Error::ContentNotAvailable) => None,
                Err(err) => return Err(platform_err(&err)),
            }
        };
        // Copy the *raw* clipboard image bytes (registered "PNG" then
        // `CF_DIBV5`, the same order arboard's `get_image` honours) out under
        // the lock, but defer the decode + PNG re-encode to `finalize_capture`
        // outside the read timeout. The rest of the pipeline (storage, search
        // snippets, IPC, copy-back) still sees `image/png` once finalised, the
        // same way macOS publishes it straight off the pasteboard.
        //
        // Probe the published image dimensions before copying any bytes:
        // a `CF_DIBV5` is uncompressed (`width × height × 4`), so a
        // pathological 65535×65535 header would otherwise have us copy a
        // multi-GB payload here. `image_pixel_overflow` reads the dimensions
        // from the header prefix and lets us skip the image rep — continuing
        // with whatever text / file-list also rode on this snapshot — before
        // the copy. The off-timeout decode is then bounded too.
        #[cfg(windows)]
        let image: Option<RawImage> = match win::image_pixel_overflow(MAX_DECODED_IMAGE_PIXELS) {
            Some(observed) => {
                tracing::warn!(
                    observed_pixels = observed,
                    max_pixels = MAX_DECODED_IMAGE_PIXELS,
                    "image_rep_dropped reason=decoded_pixels_exceed_cap"
                );
                None
            }
            // Propagate a transient read failure (`?`) so the capture retries
            // rather than committing a text-only snapshot — and consuming its
            // sequence — while an image that should have been read is lost.
            None => win::read_image_payload()?,
        };
        #[cfg(not(windows))]
        let image: Option<RawImage> = None;
        // Drop the arboard guard before the second Win32 read so we don't hold
        // it across the CF_HDROP OpenClipboard call; the sequence-stability
        // check is what protects us against a write landing in between.
        drop(guard);

        // Assemble the file-list and text reps and record where the image rep
        // belongs (after the file list, before text) so the deferred decode can
        // splice the PNG back at the same position, preserving representation
        // order and the dedup `representation_set_hash`.
        let (representations, image_index) = assemble_text_and_files(plain);

        // Re-probe the exclusion marker after the body read: a marker can race
        // in within a single clear-then-write publish that the pre-read probe
        // missed (or could not open the clipboard to see). Mirrors the macOS
        // post-read re-check — the just-read `representations`, including any
        // secret body, are dropped unreturned. Reusing `owner_exclusion_probe`
        // gives the same settle/retry handling, so an inconclusive probe retries
        // rather than committing a body it could not screen.
        #[cfg(windows)]
        match owner_exclusion_probe(before, attempt) {
            OwnerExclusionProbe::Settled(snapshot) => return Ok((snapshot, None)),
            OwnerExclusionProbe::Retry => continue,
            OwnerExclusionProbe::Absent => {}
        }

        let after = native_sequence_number();
        if before == after || attempt >= MAX_RETRIES {
            let snapshot = ClipboardSnapshot {
                sequence: ClipboardSequence::native(i64::from(after)),
                captured_at: OffsetDateTime::now_utc(),
                source: None,
                representations,
            };
            // The post-encode `total_payload_bytes` size gate moves to
            // `finalize_capture`, which knows the encoded image size.
            return Ok((
                CapturedSnapshot::Captured(snapshot),
                image.map(|img| (image_index, img)),
            ));
        }
    }
}

/// Decode a deferred raw image to RGBA and PNG-encode it outside the read
/// timeout, splice it into the captured representations at its recorded
/// index, and apply the entry-size budget to the full payload.
///
/// Both the DIB/PNG -> RGBA decode and the PNG encode are CPU-bound and touch
/// neither the clipboard nor its mutex, so they run on a plain blocking task
/// here — mirroring the write path's `write_image_bytes`, which already
/// decodes off the timed section. `win::oversized_payload` deliberately never
/// sizes raw `CF_DIBV5` (it is uncompressed and routinely several MiB for
/// ordinary screenshots), so the normalised image size is first known here;
/// surfacing it as `Oversized` preserves the gate the in-timed per-kind size
/// check used to apply.
async fn finalize_capture(
    captured: CapturedSnapshot,
    image: Option<(usize, RawImage)>,
    budget: Option<ReadBudget>,
) -> Result<CapturedSnapshot> {
    let CapturedSnapshot::Captured(snapshot) = captured else {
        // `Oversized` was already decided on raw clipboard sizes.
        return Ok(captured);
    };
    let encoded = if let Some((index, image)) = image {
        let png = tokio::task::spawn_blocking(move || decode_raw_image_to_png(image))
            .await
            .map_err(|err| AppError::Platform(err.to_string()))?;
        // A decode/encode failure is deterministic (the raw bytes were already
        // copied), so retrying would wedge capture on the same payload every
        // tick. Drop just the image and keep the rest of the snapshot — the
        // same way the macOS adapter drops an undecodable TIFF. A *transient*
        // read failure is handled earlier, in `capture_snapshot`, by an `Err`.
        png.map(|png| (index, png))
    } else {
        None
    };
    Ok(assemble_capture(snapshot, encoded, budget))
}

/// Raw clipboard image bytes copied out under the clipboard lock but not yet
/// decoded. Carried out of [`capture_snapshot`] so the CPU-bound decode +
/// PNG encode can run outside [`CLIPBOARD_OP_TIMEOUT`].
// Only `win::read_image_payload` (Windows-only) constructs these; the decode
// helpers compile on every target so `finalize_capture` stays platform-
// agnostic.
#[cfg_attr(not(windows), allow(dead_code))]
enum RawImage {
    /// Registered `"PNG"` clipboard format — already an encoded PNG.
    Png(Vec<u8>),
    /// `CF_DIBV5` payload: a `BITMAPV5HEADER` followed by pixel data.
    Dibv5(Vec<u8>),
}

/// Decode a raw clipboard image to RGBA, then PNG-encode it.
///
/// Faithfully mirrors arboard's `read_png` / `read_cf_dibv5` (both decode to
/// RGBA through the same `image`-crate decoders this calls) followed by this
/// adapter's [`encode_rgba_to_png`], so the stored bytes are byte-identical
/// to the previous in-arboard `get_image` path — only moved off the read
/// timeout.
fn decode_raw_image_to_png(image: RawImage) -> Option<Vec<u8>> {
    encode_rgba_to_png(decode_raw_image_to_rgba(image)?)
}

fn decode_raw_image_to_rgba(image: RawImage) -> Option<ImageData<'static>> {
    use image::codecs::bmp::BmpDecoder;
    use image::codecs::png::PngDecoder;

    let (width, height, rgba) = match image {
        RawImage::Png(bytes) => decode_within_pixel_cap(PngDecoder::new(Cursor::new(bytes)).ok()?)?,
        RawImage::Dibv5(mut bytes) => {
            // `CF_DIBV5` is a headerless BMP (no `BITMAPFILEHEADER`).
            maybe_tweak_dibv5_header(&mut bytes);
            decode_within_pixel_cap(BmpDecoder::new_without_file_header(Cursor::new(&bytes)).ok()?)?
        }
    };
    Some(ImageData {
        width: width as usize,
        height: height as usize,
        bytes: Cow::Owned(rgba.into_raw()),
    })
}

/// Decode to RGBA only after confirming the advertised canvas fits under
/// [`MAX_DECODED_IMAGE_PIXELS`].
///
/// `image_pixel_overflow` probes dimensions in a separate `OpenClipboard`
/// session, so it cannot bound this allocation on its own: the clipboard may
/// have flipped between the probe and the copy, the probe may have failed to
/// read a header it treats as "safe to proceed", or a low-bit-depth `CF_DIBV5`
/// under the raw-byte ceiling can still expand past the pixel cap. Re-checking
/// `decoder.dimensions()` here — before `into_rgba8` allocates `width × height
/// × 4` — makes the RGBA buffer strictly bounded, matching the macOS adapter's
/// `decode_rgba_with_pixel_cap`.
fn decode_within_pixel_cap<D: image::ImageDecoder>(
    decoder: D,
) -> Option<(u32, u32, image::RgbaImage)> {
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) > MAX_DECODED_IMAGE_PIXELS {
        tracing::warn!(
            width,
            height,
            max_pixels = MAX_DECODED_IMAGE_PIXELS,
            "image_rep_dropped reason=decoded_pixels_exceed_cap"
        );
        return None;
    }
    let rgba = image::DynamicImage::from_decoder(decoder)
        .ok()?
        .into_rgba8();
    Some((width, height, rgba))
}

/// Replicate arboard's `maybe_tweak_header`: a 32-bit `BI_RGB` `CF_DIBV5`
/// whose alpha mask is `0xff000000` is reinterpreted as `BI_BITFIELDS` (and
/// given default channel masks when they are absent) so the `image`-crate BMP
/// decoder reads the alpha channel the same way arboard does. Field offsets
/// follow the `BITMAPV5HEADER` layout (little-endian).
fn maybe_tweak_dibv5_header(bytes: &mut [u8]) {
    const BI_RGB: u32 = 0;
    const BI_BITFIELDS: u32 = 3;
    /// `size_of::<BITMAPV5HEADER>()`. arboard rejects a shorter `CF_DIBV5`
    /// outright; we leave it untouched and let the decoder reject it.
    const BITMAPV5HEADER_SIZE: usize = 124;
    // Offsets within BITMAPV5HEADER: bV5BitCount @14 (u16), bV5Compression
    // @16 (u32), bV5{Red,Green,Blue}Mask @40/44/48, bV5AlphaMask @52 (u32).
    if bytes.len() < BITMAPV5HEADER_SIZE {
        return;
    }
    let read_u32 =
        |b: &[u8], at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
    let bit_count = u16::from_le_bytes([bytes[14], bytes[15]]);
    let compression = read_u32(bytes, 16);
    let alpha_mask = read_u32(bytes, 52);
    if bit_count == 32 && compression == BI_RGB && alpha_mask == 0xff00_0000 {
        bytes[16..20].copy_from_slice(&BI_BITFIELDS.to_le_bytes());
        if read_u32(bytes, 40) == 0 && read_u32(bytes, 44) == 0 && read_u32(bytes, 48) == 0 {
            bytes[40..44].copy_from_slice(&0x00ff_0000u32.to_le_bytes());
            bytes[44..48].copy_from_slice(&0x0000_ff00u32.to_le_bytes());
            bytes[48..52].copy_from_slice(&0x0000_00ffu32.to_le_bytes());
        }
    }
}

/// Splice the already-encoded image PNG back into `snapshot` at its recorded
/// index and apply the entry-size budget. Kept synchronous and pure so the
/// representation-order preservation and the oversize gate are unit-testable
/// without a runtime (the CPU-bound encode happens in `finalize_capture`).
fn assemble_capture(
    mut snapshot: ClipboardSnapshot,
    image: Option<(usize, Vec<u8>)>,
    budget: Option<ReadBudget>,
) -> CapturedSnapshot {
    if let Some((index, png)) = image {
        let index = index.min(snapshot.representations.len());
        snapshot.representations.insert(
            index,
            ClipboardRepresentation {
                mime_type: "image/png".to_owned(),
                data: ClipboardData::Bytes(png),
            },
        );
    }
    if let Some(budget) = budget
        && let Some((observed_bytes, limit)) = oversized_kind(&snapshot, budget)
    {
        return CapturedSnapshot::Oversized {
            sequence: snapshot.sequence,
            observed_bytes,
            limit,
        };
    }
    CapturedSnapshot::Captured(snapshot)
}

/// Report the first representation that overflows its content kind's budget,
/// as `(observed_bytes, limit)`.
///
/// Each representation is sized individually — image bytes (mime `image/*` or a
/// raw byte payload) against `budget.image_bytes`, text / file-list bytes
/// against `budget.text_bytes`. The per-kind *sum* is deliberately not enforced
/// here: a primary that fits plus an alternative that fits must reach the
/// capture loop, where `trim_alternatives_to_budget` drops the alternative and
/// keeps the primary (matching the macOS / Linux adapters). This is the
/// authoritative post-encode size check — the registered "PNG" format is sized
/// pre-read, but a DIB only becomes a PNG of known size here.
fn oversized_kind(snapshot: &ClipboardSnapshot, budget: ReadBudget) -> Option<(usize, usize)> {
    snapshot.representations.iter().find_map(|rep| {
        let bytes = match &rep.data {
            ClipboardData::Text(text) => text.len(),
            ClipboardData::Bytes(bytes) => bytes.len(),
            ClipboardData::FilePaths(paths) => paths.iter().map(String::len).sum(),
        };
        let limit =
            if rep.mime_type.starts_with("image/") || matches!(rep.data, ClipboardData::Bytes(_)) {
                budget.image_bytes
            } else {
                budget.text_bytes
            };
        (bytes > limit).then_some((bytes, limit))
    })
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
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::{char, mem, slice};

    use nagori_core::ReadBudget;

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

    use nagori_core::{
        AppError, MAX_DECODED_IMAGE_PIXELS, RepresentationDataRef, Result,
        StoredClipboardRepresentation,
    };
    use nagori_platform::ClipboardExclusionKind;

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

    pub(super) fn oversized_payload(budget: ReadBudget) -> Option<(usize, usize)> {
        // SAFETY: every successful `OpenClipboard` is paired with the
        // `ClipboardGuard` drop path. `GetClipboardData` handles are borrowed
        // from the OS-owned clipboard and are only inspected while the
        // clipboard remains open.
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return None;
            }
            let _guard = ClipboardGuard;
            // Reject any *single* representation that overflows its kind's
            // budget: the text and file-list payloads each answer to the text
            // budget, the registered "PNG" format to the image budget. The two
            // text-kind representations are checked individually rather than
            // summed — a primary that fits plus an alternative that fits must
            // reach the capture loop, where `trim_alternatives_to_budget` drops
            // the alternative and keeps the primary; summing here would drop the
            // whole clip and diverge from the macOS / Linux adapters. Returns
            // `(observed, limit)` for the first representation that overflows.
            if IsClipboardFormatAvailable(u32::from(CF_UNICODETEXT)) != 0
                && let Some(text_bytes) = unicode_text_utf8_len(budget.text_bytes)
                && text_bytes > budget.text_bytes
            {
                return Some((text_bytes, budget.text_bytes));
            }
            if IsClipboardFormatAvailable(u32::from(CF_HDROP)) != 0
                && let Some(file_list_bytes) = global_data_size(u32::from(CF_HDROP))
                && file_list_bytes > budget.text_bytes
            {
                return Some((file_list_bytes, budget.text_bytes));
            }
            // Skip CF_DIB / CF_DIBV5 here. Raw DIB is uncompressed
            // (~width*height*4 bytes) and routinely several MiB for
            // ordinary screenshots that fit comfortably under the image
            // budget once we RGBA -> PNG encode in `capture_snapshot`. The
            // post-encode `oversized_kind` check is the authoritative limit,
            // and `image_pixel_overflow` still rejects pathological
            // dimensions before the RGBA allocation. The registered "PNG"
            // format, however, is *already* encoded, so its raw size is a
            // truthful preview of what will land in storage — keep that
            // probe so a small-dimensioned but multi-MB PNG bails out
            // before arboard reads the full payload into an RGBA buffer.
            if let Some(png_id) = png_format_id()
                && IsClipboardFormatAvailable(png_id) != 0
                && let Some(png_bytes) = global_data_size(png_id)
                && png_bytes > budget.image_bytes
            {
                return Some((png_bytes, budget.image_bytes));
            }
            None
        }
    }

    /// Whether the `CF_UNICODETEXT` payload's UTF-8 byte length exceeds
    /// `ceiling`, probed via `GlobalSize` before the text is copied out.
    ///
    /// Backs the unbounded `current_snapshot` path's defence-in-depth text
    /// ceiling (the bounded path uses [`oversized_payload`]). Opens its own
    /// clipboard session, exactly like `oversized_payload`, so it is safe to
    /// call before arboard's `get_text` (which manages its own session).
    pub(super) fn unicode_text_over_ceiling(ceiling: usize) -> bool {
        // SAFETY: the `OpenClipboard` is paired with the `ClipboardGuard`
        // drop, and the borrowed handle is only inspected while the clipboard
        // is open.
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return false;
            }
            let _guard = ClipboardGuard;
            if IsClipboardFormatAvailable(u32::from(CF_UNICODETEXT)) == 0 {
                return false;
            }
            unicode_text_utf8_len(ceiling).is_some_and(|len| len > ceiling)
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

    /// Well-known Windows clipboard format *names* a clipboard owner sets to
    /// declare "do not record this in history" — the cross-platform analogue
    /// of the macOS nspasteboard.org markers.
    ///
    /// - `Clipboard Viewer Ignore` is the long-standing de-facto convention
    ///   honoured by clipboard managers; password managers (`KeePass`, …) set
    ///   it when copying a credential.
    /// - `ExcludeClipboardContentFromMonitorProcessing` is Microsoft's
    ///   documented format for excluding content from clipboard monitoring /
    ///   history, set by modern password managers and security-conscious apps.
    ///
    /// Both are *presence-only* secret markers: only the format's availability
    /// matters, never its data, so we never pull the (possibly secret) payload
    /// into our address space — mirroring the macOS adapter's
    /// `availableTypeFromArray` presence test. Neither has a transient
    /// analogue, so both surface as [`ClipboardExclusionKind::Concealed`].
    const EXCLUSION_FORMAT_NAMES: &[&str] = &[
        "Clipboard Viewer Ignore",
        "ExcludeClipboardContentFromMonitorProcessing",
    ];

    /// Register a clipboard format *name* and return its session-stable id.
    ///
    /// Generalises [`png_format_id`] to an arbitrary UTF-8 name by widening it
    /// to a NUL-terminated UTF-16 buffer. A registration failure (out of
    /// clipboard-format slots) is treated as "format absent" so the caller
    /// falls through rather than reporting a spurious match.
    unsafe fn register_clipboard_format(name: &str) -> Option<u32> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let id = unsafe { RegisterClipboardFormatW(wide.as_ptr()) };
        (id != 0).then_some(id)
    }

    /// Outcome of an owner-exclusion presence probe.
    ///
    /// `Unavailable` is kept distinct from `Absent` because the probe opens its
    /// own clipboard session: a momentarily-locked clipboard (another app
    /// mid-publish) returns `Unavailable`, which the caller must treat as
    /// inconclusive — *not* "no marker" — so a marked secret published behind a
    /// transient lock is never read as if it were unmarked.
    pub(super) enum MarkerProbe {
        Present(ClipboardExclusionKind),
        Absent,
        Unavailable,
    }

    /// Probe the clipboard for an owner-declared exclusion marker, reporting
    /// `Present` when one of the [`EXCLUSION_FORMAT_NAMES`] is offered.
    ///
    /// Opens its own short clipboard session (like [`oversized_payload`] /
    /// [`unicode_text_over_ceiling`]) and only calls `IsClipboardFormatAvailable`
    /// — it never reads any handle's bytes, so a marked secret is skipped before
    /// `get_text` ever copies it. A failed `OpenClipboard` is `Unavailable`
    /// (inconclusive), not `Absent`, so the caller retries rather than reading
    /// a body it could not screen. Runs before *and after* the body read in
    /// [`capture_snapshot`], matching the macOS adapter's pre/post exclusion
    /// checks.
    pub(super) fn owner_exclusion() -> MarkerProbe {
        // SAFETY: the `OpenClipboard` is paired with the `ClipboardGuard`
        // drop, and we only call `IsClipboardFormatAvailable` (a pure
        // presence query) while the clipboard is open — no handle is locked.
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return MarkerProbe::Unavailable;
            }
            let _guard = ClipboardGuard;
            for name in EXCLUSION_FORMAT_NAMES {
                if let Some(id) = register_clipboard_format(name)
                    && IsClipboardFormatAvailable(id) != 0
                {
                    return MarkerProbe::Present(ClipboardExclusionKind::Concealed);
                }
            }
            MarkerProbe::Absent
        }
    }

    unsafe fn global_data_size(format: u32) -> Option<usize> {
        let handle = unsafe { GetClipboardData(format) };
        if handle.is_null() {
            return None;
        }
        let bytes = unsafe { GlobalSize(handle) };
        (bytes > 0).then_some(bytes)
    }

    /// UTF-8 byte length of the `CF_UNICODETEXT` payload, capped at `cap`.
    ///
    /// Sums UTF-8 lengths up to the first NUL, short-circuiting as soon as the
    /// running total exceeds `cap`. Both callers only compare against a
    /// threshold, so the exact length past `cap` is irrelevant — and stopping
    /// early keeps a multi-GB clip without a NUL terminator from forcing a full
    /// UTF-16 decode under the clipboard lock (and inside the read timeout).
    /// Each non-NUL UTF-16 unit yields >= 1 UTF-8 byte, so the loop visits at
    /// most `cap + 1` units before the threshold is crossed.
    unsafe fn unicode_text_utf8_len(cap: usize) -> Option<usize> {
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
        let mut utf8_len = 0_usize;
        for decoded in char::decode_utf16(wide.iter().copied().take_while(|unit| *unit != 0)) {
            utf8_len =
                utf8_len.saturating_add(decoded.unwrap_or(char::REPLACEMENT_CHARACTER).len_utf8());
            if utf8_len > cap {
                break;
            }
        }
        let _ = unsafe { GlobalUnlock(handle) };
        Some(utf8_len)
    }

    /// Hard ceiling on a raw clipboard image payload we will copy out.
    ///
    /// A valid image within [`MAX_DECODED_IMAGE_PIXELS`] occupies at most
    /// `width × height × 4` uncompressed bytes (a `CF_DIBV5`) plus its header;
    /// a PNG of the same canvas is smaller. `image_pixel_overflow` rejects
    /// oversized *dimensions* before we get here, but it runs in a separate
    /// `OpenClipboard` session, so a clipboard that flips to a huge image (or
    /// a crafted small-dimension payload backed by a padded `HGLOBAL`) between
    /// the probe and this copy would otherwise force an unbounded allocation.
    /// This cap bounds the copy regardless of what the probe saw — the
    /// sequence-stability retry then discards a payload that changed under us.
    const MAX_RAW_IMAGE_BYTES: u64 = MAX_DECODED_IMAGE_PIXELS * 4 + 4096;

    /// Copy the bytes backing a clipboard global-memory handle, bounded by
    /// [`MAX_RAW_IMAGE_BYTES`].
    ///
    /// `Err` on a genuine (possibly transient) read failure — the format was
    /// advertised as available but the handle / lock could not be obtained —
    /// so the caller propagates it and the capture retries instead of
    /// silently dropping an image that should have been read. `Ok(None)` when
    /// the payload exceeds the ceiling (skip the oversized image, keep the
    /// rest of the snapshot).
    unsafe fn read_bounded_image_bytes(format: u32) -> Result<Option<Vec<u8>>> {
        let handle = unsafe { GetClipboardData(format) };
        if handle.is_null() {
            return Err(AppError::Platform(
                "clipboard image handle was unavailable".to_owned(),
            ));
        }
        let size = unsafe { GlobalSize(handle) };
        if size == 0 {
            return Err(AppError::Platform(
                "clipboard image payload was empty".to_owned(),
            ));
        }
        if size as u64 > MAX_RAW_IMAGE_BYTES {
            tracing::warn!(
                byte_count = size,
                ceiling = MAX_RAW_IMAGE_BYTES,
                "image_rep_dropped reason=raw_payload_exceeds_ceiling"
            );
            return Ok(None);
        }
        let locked = unsafe { GlobalLock(handle) };
        if locked.is_null() {
            return Err(AppError::Platform("clipboard image lock failed".to_owned()));
        }
        let bytes = unsafe { slice::from_raw_parts(locked.cast::<u8>(), size) }.to_vec();
        let _ = unsafe { GlobalUnlock(handle) };
        Ok(Some(bytes))
    }

    /// Copy the raw clipboard image payload — the registered `"PNG"` format
    /// first, then `CF_DIBV5` — mirroring the lookup order arboard's
    /// `get_image` honours. Returns the bytes uninterpreted; the decode to
    /// RGBA + PNG re-encode happens off the read timeout in `finalize_capture`
    /// so a large image cannot wedge the clipboard read. The copy is bounded
    /// by [`MAX_RAW_IMAGE_BYTES`]. `Err` surfaces a transient read failure for
    /// an advertised format so the capture retries rather than losing the
    /// image; `Ok(None)` means no image format is present (or it was over the
    /// ceiling).
    pub(super) fn read_image_payload() -> Result<Option<super::RawImage>> {
        // SAFETY: `OpenClipboard(null)` attaches to the calling thread and the
        // `ClipboardGuard` closes it on every return path. The borrowed
        // `GetClipboardData` handles are only read while the clipboard stays
        // open (inside `read_bounded_image_bytes`, before the guard drops).
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return Err(AppError::Platform(
                    "OpenClipboard failed for image read".to_owned(),
                ));
            }
            let _guard = ClipboardGuard;
            if let Some(png_id) = png_format_id()
                && IsClipboardFormatAvailable(png_id) != 0
            {
                return Ok(read_bounded_image_bytes(png_id)?.map(super::RawImage::Png));
            }
            if IsClipboardFormatAvailable(u32::from(CF_DIBV5)) != 0 {
                return Ok(
                    read_bounded_image_bytes(u32::from(CF_DIBV5))?.map(super::RawImage::Dibv5)
                );
            }
            Ok(None)
        }
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
        for (index, path) in paths.iter().enumerate() {
            let encoded: Vec<u16> = OsString::from(path).encode_wide().collect();
            // Identify the offending entry by index / length only — never echo
            // the path itself, which can be sensitive ("length only, never
            // content").
            if encoded.contains(&0) {
                return Err(AppError::Unsupported(format!(
                    "file path at index {index} contains an interior NUL; cannot publish as CF_HDROP",
                )));
            }
            if encoded.len() >= MAX_PATH_WCHARS as usize {
                return Err(AppError::Unsupported(format!(
                    "file path at index {index} ({} wchars) exceeds the Win32 long-path limit",
                    encoded.len(),
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
        // applies here as in the single-image path. The shared helper probes
        // dimensions before the `decode` call materialises the RGBA buffer.
        let rgba = nagori_platform::decode_rgba_with_pixel_cap(encoded, MAX_DECODED_IMAGE_PIXELS)
            .map_err(|err| super::decode_err_to_app_error(&err))?;
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

#[cfg(test)]
mod tests {
    use image::ImageReader;

    use super::*;

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
    fn decode_err_to_app_error_maps_forged_canvas_above_cap_to_unsupported() {
        // Forged PNG that advertises a pixel count above
        // MAX_DECODED_IMAGE_PIXELS but encodes to a few-dozen bytes —
        // exactly the asymmetric payload a decompression-bomb guard must
        // reject. The shared decode must refuse it pre-decode and this
        // adapter must surface the refusal as Unsupported so
        // `write_image_bytes` / `build_dibv5_payload` report it upward.
        let dim = dim_above_cap();
        let forged = forge_png_header(dim, dim);
        let err = nagori_platform::decode_rgba_with_pixel_cap(&forged, MAX_DECODED_IMAGE_PIXELS)
            .map_err(|err| decode_err_to_app_error(&err))
            .expect_err("must reject above cap");
        assert!(matches!(err, AppError::Unsupported(_)), "got {err:?}");
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
    fn oversized_kind_sizes_each_representation_against_its_own_budget() {
        // Image bytes answer to the image budget; text and file-list bytes each
        // answer to the text budget *individually* — the per-kind sum is left to
        // the capture loop's trim so a primary + alternative that each fit are
        // not dropped wholesale here.
        let snapshot = snapshot_with(vec![
            ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text("hello".to_owned()), // 5 (text)
            },
            ClipboardRepresentation {
                mime_type: "image/png".to_owned(),
                data: ClipboardData::Bytes(vec![0u8; 10]), // 10 (image)
            },
            ClipboardRepresentation {
                mime_type: "text/uri-list".to_owned(),
                data: ClipboardData::FilePaths(vec!["abc".to_owned(), "de".to_owned()]), // 5 (text)
            },
        ]);
        // Every representation fits its own budget.
        assert_eq!(oversized_kind(&snapshot, ReadBudget::new(10, 10)), None);
        // The image overflows the image budget, even though text fits.
        assert_eq!(
            oversized_kind(&snapshot, ReadBudget::new(10, 9)),
            Some((10, 9))
        );
        // A single text representation over the text budget is reported.
        assert_eq!(
            oversized_kind(&snapshot, ReadBudget::new(4, 10)),
            Some((5, 4))
        );
        // The two text-kind reps sum to 10, but each (5) is under a budget of 9,
        // so the clip is *not* rejected — trim handles the aggregate downstream.
        assert_eq!(oversized_kind(&snapshot, ReadBudget::new(9, 10)), None);
    }

    #[test]
    fn oversized_kind_is_none_for_an_empty_snapshot() {
        assert_eq!(
            oversized_kind(&snapshot_with(Vec::new()), ReadBudget::new(1, 1)),
            None
        );
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

    #[test]
    fn decode_raw_image_to_png_round_trips_a_png_payload() {
        // The deferred decode path: a registered-"PNG" clipboard payload is
        // decoded to RGBA and re-encoded to PNG (matching arboard's
        // get_image -> read_png -> RGBA, then this adapter's encode), all off
        // the read timeout. The pixels must survive the round-trip.
        let source = encode_rgba_to_png(ImageData {
            width: 2,
            height: 1,
            bytes: Cow::Owned(vec![
                0xFF, 0x00, 0x00, 0xFF, // red
                0x00, 0xFF, 0x00, 0xFF, // green
            ]),
        })
        .expect("seed PNG encodes");

        let png = decode_raw_image_to_png(RawImage::Png(source))
            .expect("PNG payload decodes + re-encodes");
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
    fn maybe_tweak_dibv5_header_promotes_bi_rgb_with_alpha_mask() {
        // arboard reinterprets a 32-bit BI_RGB DIBV5 carrying an alpha mask as
        // BI_BITFIELDS and fills in default channel masks so the BMP decoder
        // reads alpha. Replicate that exactly: build a 124-byte header with
        // bitCount=32, compression=BI_RGB, alphaMask=0xff000000, zero RGB
        // masks.
        let mut header = vec![0u8; 124];
        header[14..16].copy_from_slice(&32u16.to_le_bytes()); // bV5BitCount
        header[16..20].copy_from_slice(&0u32.to_le_bytes()); // BI_RGB
        header[52..56].copy_from_slice(&0xff00_0000u32.to_le_bytes()); // bV5AlphaMask

        maybe_tweak_dibv5_header(&mut header);

        let read = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
        assert_eq!(read(16), 3, "compression must become BI_BITFIELDS");
        assert_eq!(read(40), 0x00ff_0000, "default red mask");
        assert_eq!(read(44), 0x0000_ff00, "default green mask");
        assert_eq!(read(48), 0x0000_00ff, "default blue mask");
    }

    #[test]
    fn maybe_tweak_dibv5_header_leaves_other_headers_untouched() {
        // A 24-bit header (no alpha mask trigger) must be passed through
        // verbatim so non-alpha bitmaps decode exactly as before.
        let mut header = vec![0u8; 124];
        header[14..16].copy_from_slice(&24u16.to_le_bytes());
        let original = header.clone();
        maybe_tweak_dibv5_header(&mut header);
        assert_eq!(header, original);
    }

    fn rep(mime: &str, data: ClipboardData) -> ClipboardRepresentation {
        ClipboardRepresentation {
            mime_type: mime.to_owned(),
            data,
        }
    }

    #[test]
    fn assemble_capture_splices_image_at_recorded_index() {
        // The deferred encode reinserts the PNG after the file list and
        // before text — the same order `capture_snapshot` would have built —
        // so the dedup `representation_set_hash` is unchanged by deferral.
        let snapshot = snapshot_with(vec![
            rep(
                "text/uri-list",
                ClipboardData::FilePaths(vec!["C:/a.txt".to_owned()]),
            ),
            rep("text/plain", ClipboardData::Text("body".to_owned())),
        ]);
        let assembled = assemble_capture(
            snapshot,
            Some((1, vec![1, 2, 3, 4])),
            Some(ReadBudget::new(1024, 1024)),
        );
        let CapturedSnapshot::Captured(out) = assembled else {
            panic!("expected captured");
        };
        let order: Vec<&str> = out
            .representations
            .iter()
            .map(|r| r.mime_type.as_str())
            .collect();
        assert_eq!(order, ["text/uri-list", "image/png", "text/plain"]);
    }

    #[test]
    fn assemble_capture_reports_oversized_after_encode() {
        // `oversized_payload` never sizes raw DIB, so an image whose encoded
        // PNG blows the budget is only caught here, after the off-timeout
        // encode — surfaced as `Oversized`, matching the old in-timed gate.
        let snapshot = snapshot_with(vec![rep(
            "text/plain",
            ClipboardData::Text("hi".to_owned()),
        )]);
        // The spliced PNG (64 bytes) is image-kind, so it is sized against the
        // image budget (32); the 2-byte text stays under the text budget.
        let assembled = assemble_capture(
            snapshot,
            Some((0, vec![0u8; 64])),
            Some(ReadBudget::new(32, 32)),
        );
        match assembled {
            CapturedSnapshot::Oversized {
                observed_bytes,
                limit,
                ..
            } => {
                assert_eq!(limit, 32);
                assert!(observed_bytes > 32);
            }
            other => panic!("expected oversized, got {other:?}"),
        }
    }

    #[test]
    fn assemble_capture_without_max_never_reports_oversized() {
        let snapshot = snapshot_with(vec![rep(
            "text/plain",
            ClipboardData::Text("hi".to_owned()),
        )]);
        let assembled = assemble_capture(snapshot, Some((0, vec![0u8; 4096])), None);
        assert!(matches!(assembled, CapturedSnapshot::Captured(_)));
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
