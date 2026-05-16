//! Magic-number detection for raster image payloads received over the
//! clipboard.
//!
//! Producers (browsers, chat apps, malicious sites) freely attach
//! `image/...` MIME metadata to arbitrary bytes. The custom-scheme
//! handler in the desktop app already sets `X-Content-Type-Options:
//! nosniff`, but defence-in-depth wants the *capture* boundary to check
//! that the bytes start with a recognised signature before we persist
//! them. The detector here is the single source of truth for that check
//! — both the entry factory (write side) and the Tauri image scheme
//! handler (read side) consult it so a bypass in one path still trips
//! on the other.
//!
//! The set of supported formats matches the desktop allow-list
//! (`ALLOWED_IMAGE_MIME`); SVG is intentionally absent because it can
//! host script.

/// Image formats the workspace recognises by their byte signature.
///
/// Variants are mapped to a canonical IANA MIME via [`Self::mime_type`]
/// so call sites can compare against the desktop allow-list without
/// duplicating the string table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    WebP,
    Bmp,
    Tiff,
}

impl ImageFormat {
    /// Canonical IANA MIME for the format. Matches the lowercase form
    /// used by `sanitise_image_mime` in the desktop crate so equality
    /// comparison "just works" across boundaries.
    #[must_use]
    pub const fn mime_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::WebP => "image/webp",
            Self::Bmp => "image/bmp",
            Self::Tiff => "image/tiff",
        }
    }
}

/// Identify an image format from its leading bytes.
///
/// Returns `None` when the buffer is too short to contain any known
/// signature, or when the prefix doesn't match anything in the
/// allow-list. The function is total — it never reads past
/// `bytes.len()` — so callers can hand it truncated or even empty
/// slices without preflight checks.
#[must_use]
pub fn detect(bytes: &[u8]) -> Option<ImageFormat> {
    // PNG — fixed 8-byte signature defined by RFC 2083 §3.1.
    if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some(ImageFormat::Png);
    }
    // JPEG — every Start of Image marker is `FF D8 FF`. The fourth byte
    // varies (E0 for JFIF, E1 for EXIF, DB for raw JPEG-LS, …); we only
    // need the SOI to be confident the payload is JPEG.
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(ImageFormat::Jpeg);
    }
    // GIF — "GIF87a" or "GIF89a", per the GIF spec.
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(ImageFormat::Gif);
    }
    // WebP — RIFF container with the "WEBP" four-character code at
    // offset 8. The four bytes between RIFF and WEBP encode the
    // little-endian chunk length and are payload-dependent, so we
    // compare the marker positions rather than a flat prefix.
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(ImageFormat::WebP);
    }
    // BMP — "BM" magic.
    if bytes.starts_with(b"BM") {
        return Some(ImageFormat::Bmp);
    }
    // TIFF — little-endian (`II*\0`) or big-endian (`MM\0*`) header.
    if bytes.starts_with(&[0x49, 0x49, 0x2A, 0x00]) || bytes.starts_with(&[0x4D, 0x4D, 0x00, 0x2A])
    {
        return Some(ImageFormat::Tiff);
    }
    None
}

/// Check whether the declared MIME agrees with the byte signature.
///
/// Returns `true` only when [`detect`] recognises the payload *and* the
/// detected canonical MIME equals the declared MIME (case-insensitive,
/// ignoring any `;parameter` suffix). Used by both the entry factory
/// and the Tauri image scheme as the single yes/no gate for storing or
/// serving the bytes.
#[must_use]
pub fn matches_declared_mime(declared_mime: &str, bytes: &[u8]) -> bool {
    let Some(detected) = detect(bytes) else {
        return false;
    };
    let bare = declared_mime
        .split(';')
        .next()
        .unwrap_or(declared_mime)
        .trim();
    bare.eq_ignore_ascii_case(detected.mime_type())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal valid PNG header (8 magic bytes — IHDR omitted; we only
    // need enough to identify the format).
    const PNG_HEADER: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    // JPEG SOI followed by an APP0/JFIF marker.
    const JPEG_HEADER: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0];
    // GIF89a header.
    const GIF_HEADER: &[u8] = b"GIF89a\x01\x00\x01\x00";
    // Synthetic WebP container: "RIFF????WEBPVP8 " — the chunk length
    // (`????`) and the VP8 chunk header don't influence detection.
    const WEBP_HEADER: &[u8] = b"RIFF\x24\x00\x00\x00WEBPVP8 ";
    const BMP_HEADER: &[u8] = b"BM\x76\x00\x00\x00";
    const TIFF_LE_HEADER: &[u8] = &[0x49, 0x49, 0x2A, 0x00];
    const TIFF_BE_HEADER: &[u8] = &[0x4D, 0x4D, 0x00, 0x2A];

    #[test]
    fn detect_recognises_each_supported_format() {
        assert_eq!(detect(PNG_HEADER), Some(ImageFormat::Png));
        assert_eq!(detect(JPEG_HEADER), Some(ImageFormat::Jpeg));
        assert_eq!(detect(GIF_HEADER), Some(ImageFormat::Gif));
        assert_eq!(detect(WEBP_HEADER), Some(ImageFormat::WebP));
        assert_eq!(detect(BMP_HEADER), Some(ImageFormat::Bmp));
        assert_eq!(detect(TIFF_LE_HEADER), Some(ImageFormat::Tiff));
        assert_eq!(detect(TIFF_BE_HEADER), Some(ImageFormat::Tiff));
    }

    #[test]
    fn detect_accepts_gif87a_in_addition_to_gif89a() {
        // Older GIF87a producers still exist in the wild (legacy
        // exporters, embedded systems); make sure the alternate magic
        // is recognised so we don't false-reject them.
        assert_eq!(detect(b"GIF87a\x01\x00"), Some(ImageFormat::Gif));
    }

    #[test]
    fn detect_rejects_html_with_image_extension() {
        // Classic attacker payload: text/html body mislabelled as
        // image/png. The signature check must reject it regardless of
        // the declared MIME.
        assert_eq!(detect(b"<!doctype html><html>...</html>"), None);
    }

    #[test]
    fn detect_rejects_truncated_or_empty_buffers() {
        // PNG signature is 8 bytes — anything shorter cannot match.
        assert_eq!(detect(&[]), None);
        assert_eq!(detect(&[0x89]), None);
        assert_eq!(detect(&[0x89, 0x50, 0x4E, 0x47]), None);
        // RIFF prefix without the "WEBP" four-character code at offset
        // 8 must not be accepted as WebP.
        assert_eq!(detect(b"RIFF\x24\x00\x00\x00WAVE"), None);
    }

    #[test]
    fn detect_rejects_unknown_binary() {
        // Arbitrary bytes that don't begin with any known signature.
        assert_eq!(
            detect(&[0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07]),
            None
        );
        // ZIP-like local header would be confused with nothing here —
        // good, we don't host archive payloads.
        assert_eq!(detect(b"PK\x03\x04"), None);
    }

    #[test]
    fn matches_declared_mime_accepts_canonical_pairs() {
        assert!(matches_declared_mime("image/png", PNG_HEADER));
        assert!(matches_declared_mime("image/jpeg", JPEG_HEADER));
        assert!(matches_declared_mime("image/gif", GIF_HEADER));
        assert!(matches_declared_mime("image/webp", WEBP_HEADER));
    }

    #[test]
    fn matches_declared_mime_normalises_case_and_parameters() {
        // IANA says the type/subtype is case-insensitive; clipboard
        // producers in the wild ship both `image/PNG` and the canonical
        // form. Parameter suffixes like `; charset=...` are legal on
        // the wire even for binary types.
        assert!(matches_declared_mime("IMAGE/PNG", PNG_HEADER));
        assert!(matches_declared_mime(
            "image/png; charset=binary",
            PNG_HEADER
        ));
        assert!(matches_declared_mime("  image/jpeg  ", JPEG_HEADER));
    }

    #[test]
    fn matches_declared_mime_rejects_format_mismatch() {
        // Declared PNG but bytes are JPEG → must not pass.
        assert!(!matches_declared_mime("image/png", JPEG_HEADER));
        // Declared JPEG but bytes are HTML → must not pass.
        assert!(!matches_declared_mime("image/jpeg", b"<!doctype html>"));
        // Declared WebP but bytes are a RIFF/WAVE audio container →
        // close-but-no-cigar; the WEBP four-character code is missing.
        assert!(!matches_declared_mime(
            "image/webp",
            b"RIFF\x24\x00\x00\x00WAVEfmt "
        ));
    }

    #[test]
    fn matches_declared_mime_rejects_unknown_or_disallowed_mime() {
        // SVG is intentionally absent from the detector — declaring it
        // should always fail, even if the bytes happen to be valid XML.
        assert!(!matches_declared_mime("image/svg+xml", b"<svg/>"));
        assert!(!matches_declared_mime(
            "application/octet-stream",
            PNG_HEADER
        ));
    }
}
