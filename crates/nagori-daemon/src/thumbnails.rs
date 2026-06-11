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
//! * The pipeline aims to derive thumbnails only for `Public` (or
//!   `Unknown`-defaulted-Public) entries. The generator re-reads the
//!   live `entries` row and skips Private / Secret / Blocked
//!   classifications as a best-effort application-layer guard, so the
//!   cached copy does not become a side-channel for classified content
//!   under the common case. The check narrows the TOCTOU window but
//!   does not close it; a hard guarantee would require a storage-side
//!   invariant (see `generate_thumbnail`).
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
    AppError, ContentKind, EntryId, EntryRepository, MAX_DECODED_IMAGE_PIXELS, Result,
    ThumbnailRecord, is_text_safe_for_default_output,
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

    /// Non-blocking admission check used *before* spawning a decode task. The
    /// permit is released when dropped, so callers hold it for the duration of
    /// decode + `put_thumbnail` and not a moment longer.
    ///
    /// Returns `None` when the global decode pool is already saturated, so the
    /// caller can skip the spawn entirely instead of detaching a task that
    /// would only park on the semaphore. A burst of misses against *distinct*
    /// entries (image-heavy scroll, prefetch sweep) is bounded to the pool size
    /// rather than piling up unbounded `tokio::spawn` tasks; a rejected request
    /// is picked up on the next fetch (the `nagori-image://thumb/` 503
    /// `Retry-After` path) once a slot frees.
    pub(crate) fn try_acquire_permit(&self) -> Option<OwnedSemaphorePermit> {
        self.permits.clone().try_acquire_owned().ok()
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

/// Generate-and-store a thumbnail for `id`.
///
/// The source is the entry's primary image payload. File lists keep their
/// primary as the joined paths (text), so for that kind the function falls
/// back to an accompanying image the clip carried alongside the file URLs
/// (e.g. a presentation that also placed a slide render on the clipboard).
///
/// Returns `Ok(Some(record))` on success, `Ok(None)` when generation was
/// skipped (no image to thumbnail, sensitivity-withheld, oversized after
/// retry), and `Err(_)` on a hard storage or decode failure that the
/// caller should log.
///
/// The sensitivity gate is re-read from the live `entries` row inside
/// this function rather than trusting a caller-supplied value. This is
/// a best-effort application-layer guard: it narrows the TOCTOU window
/// between the dispatch-layer check and the derived-row write so that
/// a stale caller-supplied classification, or a reclassification that
/// lands before this read, won't persist a `Private` / `Secret` /
/// `Blocked` thumbnail. It does not close the window — the
/// classification can still flip between this read and `put_thumbnail`,
/// and a hard invariant would have to live storage-side (e.g. a
/// conditional write keyed on the row's current sensitivity). If the
/// entry has been soft-deleted by the time we look it up the row is
/// missing, so we also skip — there is no reason to spend cycles
/// deriving cached data for a tombstoned row.
pub async fn generate_thumbnail(
    store: &SqliteStore,
    id: EntryId,
) -> Result<Option<ThumbnailRecord>> {
    let Some(entry) = store.get(id).await? else {
        return Ok(None);
    };
    // Best-effort sensitivity re-check at the application layer. The
    // dispatch path already gated this entry at enqueue time, but the
    // classification can flip (re-scan, manual reclassification, settings
    // change) between that check and the derived-row write below.
    // Re-reading the live row narrows the TOCTOU window — it does not
    // close it, since the row can still flip between this read and
    // `put_thumbnail`, and a true invariant would have to live on the
    // storage side (e.g. a conditional write keyed on sensitivity). Do
    // not remove this gate without moving the guarantee somewhere
    // stricter; the alternative is a derived-row side-channel for
    // `Private` / `Secret` / `Blocked` content.
    if !is_text_safe_for_default_output(entry.sensitivity) {
        return Ok(None);
    }
    let payload = match store.get_payload(id).await? {
        Some(payload) => Some(payload),
        // A file list keeps its primary as the joined paths (text), so it
        // never has a primary image payload — but a file copy often also
        // carried an image render of the same content (a presentation slide,
        // a document page). Use that accompanying image as the thumbnail
        // source so the preview pane can show it. Scoped to file lists: it is
        // the kind where the image is reliably a render of the copied object
        // rather than incidental clipboard noise.
        None if entry.content.kind() == ContentKind::FileList => {
            store.get_alternate_image_payload(id).await?
        }
        None => None,
    };
    let Some((bytes, _mime)) = payload else {
        return Ok(None);
    };
    // Decode + re-encode is CPU-bound; hand it to the blocking pool so
    // we don't stall tokio workers.
    let record = tokio::task::spawn_blocking(move || encode_thumbnail(&bytes))
        .await
        .map_err(|err| AppError::storage(format!("thumbnail task join failed: {err}")))?;
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

    /// The global decode cap bounds admission: once the pool is full, the
    /// next `try_acquire_permit` is *refused* (returns `None`) rather than
    /// queued — that refusal is what lets `kick_thumbnail_generation` skip the
    /// spawn instead of detaching a task that would park on the semaphore. A
    /// slot frees as soon as a held permit drops.
    #[test]
    fn thumbnail_gate_admits_only_up_to_capacity() {
        let gate = ThumbnailGate::with_capacity(2);
        let p1 = gate.try_acquire_permit().expect("first slot is free");
        let p2 = gate.try_acquire_permit().expect("second slot is free");
        // Pool saturated: a third admission must be refused, not queued.
        assert!(
            gate.try_acquire_permit().is_none(),
            "third admission must be refused while two are outstanding",
        );
        drop(p1);
        let p3 = gate
            .try_acquire_permit()
            .expect("a slot frees once a permit drops");
        drop(p2);
        drop(p3);
    }

    /// Passing `0` for `max_concurrent` would otherwise refuse every
    /// generator forever; the clamp keeps at least one decoder slot.
    #[test]
    fn thumbnail_gate_clamps_zero_capacity_to_one() {
        let gate = ThumbnailGate::with_capacity(0);
        let permit = gate
            .try_acquire_permit()
            .expect("zero capacity is clamped to one slot");
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

    /// Pin the entry-time half of the write-side sensitivity gate (see
    /// `generate_thumbnail`). The dispatch layer already short-circuits
    /// non-Public entries before this function runs; this test verifies
    /// the best-effort re-read at the start of `generate_thumbnail`
    /// also short-circuits when the live row already says non-Public,
    /// so a stale caller-supplied classification doesn't persist a
    /// `Private` / `Secret` / `Blocked` thumbnail on the entry path.
    /// The remaining TOCTOU window between this read and the actual
    /// `put_thumbnail` would need a storage-side invariant to close —
    /// out of scope for this assertion.
    #[tokio::test]
    async fn generate_thumbnail_skips_non_public_entries() {
        use nagori_core::{
            ClipboardContent, EntryFactory, EntryRepository, ImageContent, Sensitivity,
        };
        use nagori_storage::SqliteStore;

        let store = SqliteStore::open_memory().expect("memory store");

        // The contents don't matter because the sensitivity gate runs
        // before decode.
        let png_bytes = tiny_transparent_png();

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

    /// A file copy that also carried an image render — e.g. a presentation
    /// dragged from Finder that placed an `image/png` on the clipboard
    /// alongside the file URL — keeps that image as a non-primary
    /// representation, so the primary-only payload lookup never finds it.
    /// The generator must fall back to the accompanying image so the
    /// preview pane can still show a thumbnail for the file list.
    #[tokio::test]
    async fn generate_thumbnail_uses_file_list_alternate_image() {
        use nagori_core::{
            ClipboardContent, EntryFactory, EntryRepository, FileListContent,
            RepresentationDataRef, RepresentationRole, StoredClipboardRepresentation,
        };
        use nagori_storage::SqliteStore;

        let store = SqliteStore::open_memory().expect("memory store");
        let content = ClipboardContent::FileList(FileListContent {
            paths: vec!["~/Documents/deck.pptx".to_owned()],
            display_text: "~/Documents/deck.pptx".to_owned(),
        });
        let mut entry = EntryFactory::from_content(content, None, None);
        entry.pending_representations = vec![
            StoredClipboardRepresentation {
                role: RepresentationRole::Primary,
                mime_type: "text/uri-list".to_owned(),
                ordinal: 0,
                data: RepresentationDataRef::FilePaths(vec!["~/Documents/deck.pptx".to_owned()]),
            },
            StoredClipboardRepresentation {
                role: RepresentationRole::Alternative,
                mime_type: "image/png".to_owned(),
                ordinal: 1,
                data: RepresentationDataRef::DatabaseBlob(tiny_transparent_png()),
            },
        ];
        let id = store.insert(entry).await.expect("insert");

        // The primary representation is the joined paths, so the
        // primary-only lookup finds no image to thumbnail...
        assert!(
            store.get_payload(id).await.expect("payload").is_none(),
            "a file list has no primary image payload",
        );
        // ...but the generator falls back to the accompanying image.
        let record = generate_thumbnail(&store, id)
            .await
            .expect("no error")
            .expect("file-list thumbnail from the accompanying image");
        assert!(record.width <= MAX_THUMBNAIL_DIMENSION);
        assert!(record.height <= MAX_THUMBNAIL_DIMENSION);
        assert!(
            store.get_thumbnail(id).await.expect("lookup").is_some(),
            "the derived thumbnail row should be persisted",
        );
    }

    /// The accompanying-image fallback is intentionally scoped to file
    /// lists. Other text-shaped kinds can pick up an incidental image
    /// representation (a rich-text paste, a browser drag) that is not a
    /// render of the copied object, so the generator must leave them
    /// without a thumbnail.
    #[tokio::test]
    async fn generate_thumbnail_ignores_alternate_image_for_non_file_list() {
        use nagori_core::{
            ClipboardContent, EntryFactory, EntryRepository, RepresentationDataRef,
            RepresentationRole, StoredClipboardRepresentation,
        };
        use nagori_storage::SqliteStore;

        let store = SqliteStore::open_memory().expect("memory store");
        let mut entry =
            EntryFactory::from_content(ClipboardContent::text("just some copied text"), None, None);
        entry.pending_representations = vec![
            StoredClipboardRepresentation {
                role: RepresentationRole::Primary,
                mime_type: "text/plain".to_owned(),
                ordinal: 0,
                data: RepresentationDataRef::InlineText("just some copied text".to_owned()),
            },
            StoredClipboardRepresentation {
                role: RepresentationRole::Alternative,
                mime_type: "image/png".to_owned(),
                ordinal: 1,
                data: RepresentationDataRef::DatabaseBlob(tiny_transparent_png()),
            },
        ];
        let id = store.insert(entry).await.expect("insert");

        assert!(
            generate_thumbnail(&store, id)
                .await
                .expect("no error")
                .is_none(),
            "non-file-list kinds must not derive a thumbnail from an incidental image",
        );
        assert!(
            store.get_thumbnail(id).await.expect("lookup").is_none(),
            "no thumbnail row should be persisted",
        );
    }

    /// A small RGBA PNG built by the encoder so it actually decodes — used
    /// as the accompanying-image payload in the fallback tests. The
    /// sensitivity test also reuses it, where any bytes would do (the gate
    /// short-circuits before decode).
    fn tiny_transparent_png() -> Vec<u8> {
        let mut img = image::RgbaImage::new(4, 4);
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
            .expect("test PNG must encode");
        bytes
    }
}
