//! Lazy thumbnail generation for the preview pane.
//!
//! Original clipboard images may be several MB; pushing the raw bytes
//! through the `WebView` every time the user navigates between rows is the
//! single largest contributor to preview latency on `Image` entries. The
//! `entry_thumbnails` table caches a downscaled (max 512px) re-encoded
//! copy keyed by entry id, generated on demand the first time the
//! preview pane requests it.
//!
//! The pipeline is deliberately a derived cache, not a representation:
//!
//! * Only `Public` (or `Unknown`-defaulted-Public) entries produce a
//!   thumbnail. Anything classified as Private / Secret / Blocked is
//!   skipped — the cached copy must not become a side-channel that
//!   reveals classified content the rest of the system gates.
//! * The cache is regenerable from the original, so eviction is safe.
//!   A separate LRU sweep (`enforce_thumbnail_budget`) keeps total
//!   bytes inside `AppSettings::max_thumbnail_total_bytes`.
//! * Generation runs on the blocking pool because `image::Decoder` and
//!   the re-encoder are CPU-bound, and we de-duplicate concurrent
//!   requests with a shared `HashSet<EntryId>` gate so a burst of
//!   preview opens for the same row only spawns one decoder.

use std::collections::HashSet;
use std::io::Cursor;
use std::sync::{Arc, Mutex};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use image::{
    DynamicImage, ImageEncoder, ImageReader, codecs::jpeg::JpegEncoder, codecs::png::PngEncoder,
    imageops::FilterType,
};
use nagori_core::{
    AppError, EntryId, EntryRepository, MAX_DECODED_IMAGE_PIXELS, Result, ThumbnailRecord,
    is_text_safe_for_default_output,
};
use nagori_storage::SqliteStore;

/// Maximum side length (px) of the generated thumbnail.
///
/// Chosen so the downscaled image still looks sharp on a `2x` `HiDPI`
/// preview pane while the encoded byte count stays well under
/// [`MAX_THUMBNAIL_BYTES`] for typical screenshots.
pub const MAX_THUMBNAIL_DIMENSION: u32 = 512;

/// Per-thumbnail byte cap. A thumbnail that exceeds this even after the
/// quality-reduction retry is discarded so a single pathological clip
/// can't dominate the LRU budget.
pub const MAX_THUMBNAIL_BYTES: usize = 256 * 1024;

/// Default ceiling for simultaneously running thumbnail decodes.
///
/// Each decode can materialise an RGBA buffer up to
/// `MAX_DECODED_IMAGE_PIXELS` (64M px → ~256 MiB) before the resize +
/// re-encode step trims it. Capping the global concurrency keeps a
/// burst of distinct-entry misses (e.g. scrolling an image-heavy
/// history) from starving the blocking pool or pushing peak RSS into
/// the gigabyte range. The value is intentionally conservative; raising
/// it past the host CPU count yields little throughput because the
/// per-decode work is CPU-bound.
pub(crate) const DEFAULT_THUMBNAIL_CONCURRENCY: usize = 4;

/// Concurrency control for in-flight thumbnail generation.
///
/// Combines two distinct limits because they fail differently:
///
/// * **Per-entry dedupe.** Frontend layouts that pre-render preview rows
///   can fire several `nagori-image://thumb/<id>` requests for the same
///   entry within a few ms. The `HashSet<EntryId>` returns success only
///   for the first waiter, who owns the generation; subsequent callers
///   skip the spawn entirely and pick up the cached row on the next
///   fetch.
/// * **Global decode cap.** Per-entry dedupe does nothing when the
///   misses target *different* entries (image-heavy scroll, prefetch).
///   The `Semaphore` bounds the total number of concurrent decoders so
///   a burst can't pile up unbounded `tokio::spawn` tasks each ready to
///   allocate hundreds of MiB.
#[derive(Clone)]
pub(crate) struct ThumbnailGate {
    in_flight: Arc<Mutex<HashSet<EntryId>>>,
    permits: Arc<Semaphore>,
}

impl Default for ThumbnailGate {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_THUMBNAIL_CONCURRENCY)
    }
}

impl ThumbnailGate {
    pub(crate) fn with_capacity(max_concurrent: usize) -> Self {
        // A zero-capacity semaphore would deadlock every acquire, which
        // is never what the caller wants for a derived cache. Clamp to
        // at least one decoder.
        let permits = max_concurrent.max(1);
        Self {
            in_flight: Arc::new(Mutex::new(HashSet::new())),
            permits: Arc::new(Semaphore::new(permits)),
        }
    }

    pub(crate) fn try_acquire(&self, id: EntryId) -> Option<ThumbnailGateGuard> {
        let mut set = self.in_flight.lock().ok()?;
        if set.insert(id) {
            Some(ThumbnailGateGuard {
                gate: self.clone(),
                id,
            })
        } else {
            None
        }
    }

    /// Wait for a slot in the global decode pool. The permit is released
    /// when dropped, so callers hold it for the duration of decode +
    /// `put_thumbnail` and not a moment longer.
    ///
    /// The semaphore is never closed, so `acquire_owned` cannot fail in
    /// practice; the error variant is mapped onto `AppError::Storage` so
    /// the rare cancellation case still surfaces in the spawn-site log.
    pub(crate) async fn acquire_permit(&self) -> Result<OwnedSemaphorePermit> {
        self.permits.clone().acquire_owned().await.map_err(|err| {
            AppError::Storage(format!("thumbnail concurrency semaphore closed: {err}"))
        })
    }
}

pub(crate) struct ThumbnailGateGuard {
    gate: ThumbnailGate,
    id: EntryId,
}

impl Drop for ThumbnailGateGuard {
    fn drop(&mut self) {
        if let Ok(mut set) = self.gate.in_flight.lock() {
            set.remove(&self.id);
        }
    }
}

/// Generate-and-store a thumbnail for `id` from the entry's primary
/// image payload.
///
/// Returns `Ok(Some(record))` on success, `Ok(None)` when generation was
/// skipped (non-image kind, sensitivity-withheld, oversized after
/// retry), and `Err(_)` on a hard storage or decode failure that the
/// caller should log.
///
/// The sensitivity gate is re-read from the live `entries` row inside
/// this function rather than trusting a caller-supplied value. A future
/// caller that passes the wrong classification, or a classification
/// transition that happens between the dispatch-layer check and the
/// derived-row write, must not be able to persist a `Private` / `Secret`
/// / `Blocked` thumbnail. If the entry has been soft-deleted by the
/// time we look it up the row is missing, so we also skip — there is no
/// reason to spend cycles deriving cached data for a tombstoned row.
pub async fn generate_thumbnail(
    store: &SqliteStore,
    id: EntryId,
) -> Result<Option<ThumbnailRecord>> {
    let Some(entry) = store.get(id).await? else {
        return Ok(None);
    };
    if !is_text_safe_for_default_output(entry.sensitivity) {
        return Ok(None);
    }
    let Some((bytes, _mime)) = store.get_payload(id).await? else {
        return Ok(None);
    };
    // Decode + re-encode is CPU-bound; hand it to the blocking pool so
    // we don't stall tokio workers.
    let record = tokio::task::spawn_blocking(move || encode_thumbnail(&bytes))
        .await
        .map_err(|err| AppError::Storage(format!("thumbnail task join failed: {err}")))?;
    match record {
        Some(record) => {
            store.put_thumbnail(id, record.clone()).await?;
            Ok(Some(record))
        }
        None => Ok(None),
    }
}

/// Decode `bytes` as an image, downscale to fit within
/// [`MAX_THUMBNAIL_DIMENSION`], and re-encode for the preview pane.
///
/// Two passes happen before `decode()`. First a header-only dimension
/// probe rejects payloads whose advertised canvas would breach
/// [`MAX_DECODED_IMAGE_PIXELS`] — a 1 KB PNG can advertise a 16K×16K
/// canvas that would otherwise materialise a multi-GB `DynamicImage`.
/// This mirrors the same gate the Windows clipboard adapter applies on
/// copy-back.
///
/// The encode branch then forks on the source's alpha channel:
///
/// * Sources without alpha take the JPEG path (quality 85, then 60 on
///   overflow). Both passes share the resized RGB buffer so the decoder
///   runs once.
/// * Sources *with* alpha take a PNG path so the inline thumbnail and
///   the expanded original look identical — flattening to RGB would
///   bake whatever lay under transparent pixels into the preview.
///
/// Anything that still overruns [`MAX_THUMBNAIL_BYTES`] returns `None`
/// — the caller falls back to streaming the original payload on demand.
fn encode_thumbnail(bytes: &[u8]) -> Option<ThumbnailRecord> {
    if exceeds_decoded_pixel_cap(bytes) {
        return None;
    }
    let decoded = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let (orig_w, orig_h) = (decoded.width(), decoded.height());
    let (target_w, target_h) = fit_within(orig_w, orig_h, MAX_THUMBNAIL_DIMENSION);
    if decoded.color().has_alpha() {
        encode_thumbnail_png(&decoded, target_w, target_h)
    } else {
        encode_thumbnail_jpeg(&decoded, target_w, target_h)
    }
}

/// Probe the encoded header alone to decide whether the decoded canvas
/// would exceed [`MAX_DECODED_IMAGE_PIXELS`].
///
/// A probe failure (corrupt header, unknown format) returns `false` so
/// the subsequent `decode()` call still surfaces the format error — it
/// is the dimensions-exceed-cap case that needs the early bail-out so
/// the decoder never tries to materialise a forged multi-GB buffer.
fn exceeds_decoded_pixel_cap(bytes: &[u8]) -> bool {
    let Some(pixels) = pixel_count_from_encoded(bytes) else {
        return false;
    };
    pixels > MAX_DECODED_IMAGE_PIXELS
}

/// Read width × height from the format header without decoding pixels.
fn pixel_count_from_encoded(bytes: &[u8]) -> Option<u64> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let (width, height) = reader.into_dimensions().ok()?;
    Some(u64::from(width).saturating_mul(u64::from(height)))
}

fn encode_thumbnail_jpeg(
    decoded: &DynamicImage,
    target_w: u32,
    target_h: u32,
) -> Option<ThumbnailRecord> {
    let resized = decoded
        .resize(target_w, target_h, FilterType::Triangle)
        .to_rgb8();
    for quality in [85u8, 60u8] {
        let mut buffer = Vec::with_capacity(MAX_THUMBNAIL_BYTES);
        let encoder = JpegEncoder::new_with_quality(&mut buffer, quality);
        if encoder
            .write_image(
                resized.as_raw(),
                resized.width(),
                resized.height(),
                image::ExtendedColorType::Rgb8,
            )
            .is_err()
        {
            continue;
        }
        if buffer.len() <= MAX_THUMBNAIL_BYTES {
            return Some(ThumbnailRecord {
                payload: buffer,
                mime_type: "image/jpeg".to_owned(),
                width: resized.width(),
                height: resized.height(),
            });
        }
    }
    None
}

fn encode_thumbnail_png(
    decoded: &DynamicImage,
    target_w: u32,
    target_h: u32,
) -> Option<ThumbnailRecord> {
    let resized = decoded
        .resize(target_w, target_h, FilterType::Triangle)
        .to_rgba8();
    let mut buffer = Vec::with_capacity(MAX_THUMBNAIL_BYTES);
    let encoder = PngEncoder::new(&mut buffer);
    encoder
        .write_image(
            resized.as_raw(),
            resized.width(),
            resized.height(),
            image::ExtendedColorType::Rgba8,
        )
        .ok()?;
    if buffer.len() <= MAX_THUMBNAIL_BYTES {
        Some(ThumbnailRecord {
            payload: buffer,
            mime_type: "image/png".to_owned(),
            width: resized.width(),
            height: resized.height(),
        })
    } else {
        None
    }
}

/// Compute the largest `(width, height)` ≤ `max_dim` that preserves
/// the source's aspect ratio. Both dimensions are clamped to ≥ 1 so the
/// resize call never asks for a zero-pixel image.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "both dimensions are non-negative and bounded by max_dim (a u32) by construction"
)]
fn fit_within(width: u32, height: u32, max_dim: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (1, 1);
    }
    if width <= max_dim && height <= max_dim {
        return (width, height);
    }
    let (w_f, h_f) = (f64::from(width), f64::from(height));
    let max_f = f64::from(max_dim);
    let scale = (max_f / w_f).min(max_f / h_f);
    let w = ((w_f * scale).round() as u32).max(1);
    let h = ((h_f * scale).round() as u32).max(1);
    (w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The global decode cap and the per-entry dedupe gate are
    /// independent: a single semaphore permit must still be honoured
    /// even if the caller already holds a dedupe guard for a distinct
    /// entry id. This guards against a future refactor that confuses
    /// the two acquire paths.
    #[tokio::test]
    async fn thumbnail_gate_serialises_decodes_past_capacity() {
        let gate = ThumbnailGate::with_capacity(2);
        let p1 = gate.acquire_permit().await.unwrap();
        let p2 = gate.acquire_permit().await.unwrap();
        // Third acquire must block until one of the held permits drops.
        let pending = gate.acquire_permit();
        tokio::pin!(pending);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut pending)
                .await
                .is_err(),
            "third permit must wait while two are outstanding",
        );
        drop(p1);
        let p3 = pending.await.expect("permit becomes available");
        drop(p2);
        drop(p3);
    }

    /// Passing `0` for `max_concurrent` would otherwise deadlock every
    /// generator forever; the clamp keeps at least one decoder slot.
    #[tokio::test]
    async fn thumbnail_gate_clamps_zero_capacity_to_one() {
        let gate = ThumbnailGate::with_capacity(0);
        let permit = gate.acquire_permit().await.unwrap();
        drop(permit);
    }

    #[test]
    fn fit_within_preserves_aspect_ratio() {
        assert_eq!(fit_within(1024, 512, 512), (512, 256));
        assert_eq!(fit_within(512, 1024, 512), (256, 512));
        assert_eq!(fit_within(400, 200, 512), (400, 200));
        assert_eq!(fit_within(0, 100, 512), (1, 1));
    }

    #[test]
    fn encode_thumbnail_downscales_large_png() {
        // A 1024x1024 solid-colour PNG is small enough to encode in-test
        // but large enough to force the resize branch.
        let mut img = image::RgbImage::new(1024, 1024);
        for pixel in img.pixels_mut() {
            *pixel = image::Rgb([0x33, 0x66, 0x99]);
        }
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(
                img.as_raw(),
                img.width(),
                img.height(),
                image::ExtendedColorType::Rgb8,
            )
            .unwrap();

        let record = encode_thumbnail(&bytes).expect("solid PNG must encode");
        assert!(record.width <= MAX_THUMBNAIL_DIMENSION);
        assert!(record.height <= MAX_THUMBNAIL_DIMENSION);
        assert_eq!(record.mime_type, "image/jpeg");
        assert!(record.payload.len() <= MAX_THUMBNAIL_BYTES);
    }

    #[test]
    fn encode_thumbnail_rejects_corrupt_bytes() {
        assert!(encode_thumbnail(b"not an image").is_none());
    }

    /// PNG / GIF / WebP can carry alpha. JPEG cannot. The inline
    /// preview swaps the original payload out for the thumbnail, so a
    /// flatten-to-RGB path would surface RGB lurking under fully-
    /// transparent pixels and make the preview look different from the
    /// expanded original. Verify the alpha-bearing path stays on a
    /// format that preserves transparency.
    #[test]
    fn encode_thumbnail_preserves_alpha_for_rgba_sources() {
        let mut img = image::RgbaImage::new(32, 32);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            let alpha: u8 = if (x + y) % 2 == 0 { 0 } else { 0xff };
            *pixel = image::Rgba([0x33, 0x66, 0x99, alpha]);
        }
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(
                img.as_raw(),
                img.width(),
                img.height(),
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();

        let record = encode_thumbnail(&bytes).expect("alpha PNG must encode");
        assert_eq!(record.mime_type, "image/png");
        // Round-trip through the decoder and confirm at least one pixel
        // is still transparent. JPEG would have flattened these.
        let round_tripped = image::load_from_memory(&record.payload)
            .expect("re-decode")
            .to_rgba8();
        assert!(
            round_tripped.pixels().any(|p| p.0[3] < 0xff),
            "thumbnail must retain at least one transparent pixel",
        );
    }

    /// A 1 KB PNG can advertise a 30K×30K canvas through a forged IHDR
    /// without ever encoding pixels for that canvas; the decode call
    /// would then allocate gigabytes of RGBA buffer. The header-only
    /// probe must reject the payload before we ever ask `decode()` for
    /// pixels.
    #[test]
    fn encode_thumbnail_rejects_pixel_bomb_header() {
        // 30K × 30K = 900M pixels — well above `MAX_DECODED_IMAGE_PIXELS`.
        let forged = forge_png_header_for_test(30_000, 30_000);
        assert!(exceeds_decoded_pixel_cap(&forged));
        assert!(encode_thumbnail(&forged).is_none());
    }

    /// Build a PNG whose IHDR advertises `width × height` but whose
    /// IDAT is an empty zlib stream. `into_dimensions` reads the header
    /// alone and so accepts this; a full `decode()` would fail (or
    /// allocate an absurd buffer) — which is exactly the asymmetry the
    /// pixel-cap probe defends against.
    fn forge_png_header_for_test(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.push(8); // bit depth
        ihdr.push(2); // colour type (RGB)
        ihdr.push(0); // compression
        ihdr.push(0); // filter
        ihdr.push(0); // interlace
        push_png_chunk(&mut out, *b"IHDR", &ihdr);
        push_png_chunk(
            &mut out,
            *b"IDAT",
            &[0x78, 0x9C, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01],
        );
        push_png_chunk(&mut out, *b"IEND", &[]);
        out
    }

    fn push_png_chunk(out: &mut Vec<u8>, chunk_type: [u8; 4], payload: &[u8]) {
        let length = u32::try_from(payload.len()).expect("chunk payload fits in u32");
        out.extend_from_slice(&length.to_be_bytes());
        let mut typed = Vec::with_capacity(4 + payload.len());
        typed.extend_from_slice(&chunk_type);
        typed.extend_from_slice(payload);
        let crc = crc32_ieee(&typed);
        out.extend_from_slice(&typed);
        out.extend_from_slice(&crc.to_be_bytes());
    }

    /// Tiny CRC32-IEEE implementation for the forged-PNG test. The
    /// reference png decoder validates each chunk's CRC, so a hand-
    /// built header has to come with the matching checksum.
    fn crc32_ieee(data: &[u8]) -> u32 {
        let mut crc: u32 = 0xffff_ffff;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                if crc & 1 != 0 {
                    crc = (crc >> 1) ^ 0xedb8_8320;
                } else {
                    crc >>= 1;
                }
            }
        }
        !crc
    }

    /// Defence-in-depth assertion for the write-side sensitivity gate
    /// (see `generate_thumbnail`). The dispatch layer already short-
    /// circuits non-Public entries before this function runs, but we
    /// re-read the stored sensitivity here so a stale caller-supplied
    /// classification — or a classification transition that happens
    /// between the dispatch check and the derived-row write — can't
    /// persist a `Private` / `Secret` / `Blocked` thumbnail to disk.
    #[tokio::test]
    async fn generate_thumbnail_skips_non_public_entries() {
        use nagori_core::{
            ClipboardContent, EntryFactory, EntryRepository, ImageContent, Sensitivity,
        };
        use nagori_storage::SqliteStore;

        let store = SqliteStore::open_memory().expect("memory store");

        // A tiny 1x1 transparent PNG; the contents don't matter because
        // the sensitivity gate runs before decode.
        let png_bytes: Vec<u8> = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];

        for blocked in [
            Sensitivity::Private,
            Sensitivity::Secret,
            Sensitivity::Blocked,
        ] {
            let content = ClipboardContent::Image(ImageContent {
                width: Some(1),
                height: Some(1),
                byte_count: png_bytes.len(),
                mime_type: Some("image/png".to_owned()),
                pending_bytes: Some(png_bytes.clone()),
            });
            let mut entry = EntryFactory::from_content(content, None, None);
            entry.sensitivity = blocked;
            let id = store.insert(entry).await.expect("insert");

            let outcome = generate_thumbnail(&store, id).await.expect("no error");
            assert!(
                outcome.is_none(),
                "generator must skip {blocked:?} entries to avoid a derived side-channel",
            );
            assert!(
                store
                    .get_thumbnail(id)
                    .await
                    .expect("thumbnail lookup")
                    .is_none(),
                "no row should be persisted for {blocked:?} entries",
            );
        }
    }
}
