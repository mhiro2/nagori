//! Decompression-bomb-guarded decode of encoded clipboard images.
//!
//! Every adapter that turns an encoded image payload (PNG / JPEG / GIF /
//! WebP / TIFF) into raw RGBA — the macOS TIFF→PNG capture normalisation,
//! the Windows copy-back's `CF_DIBV5` rendering — faces the same asymmetric
//! threat: a few-dozen-byte encoded blob can advertise a multi-GB canvas,
//! and `image::DynamicImage::to_rgba8` allocates `width × height × 4` bytes
//! unconditionally. [`decode_rgba_with_pixel_cap`] probes the header
//! dimensions first and refuses to decode anything whose advertised canvas
//! exceeds the caller's pixel cap, so the guard policy lives in one place
//! instead of being re-implemented per adapter.

use std::io::Cursor;

use image::{ImageReader, RgbaImage};

/// Why [`decode_rgba_with_pixel_cap`] did not produce an RGBA buffer.
///
/// The variants are deliberately distinct because the adapters react
/// differently: a capture path drops the representation (optionally keeping
/// the original encoded bytes when only the re-encode failed), while a
/// copy-back path surfaces the failure to the user as an error.
#[derive(Debug)]
pub enum DecodeRgbaError {
    /// The header advertises more pixels than `max_pixels` — rejected
    /// *before* `decode` could allocate the RGBA buffer.
    PixelCapExceeded {
        /// Pixel count the header advertises.
        pixels: u64,
        /// The cap the caller supplied.
        max_pixels: u64,
    },
    /// The format / dimensions could not be read from the header. A
    /// subsequent `decode` would not succeed either, so no decode is
    /// attempted; `detail` carries the probe error for diagnostics.
    DimensionsUnreadable {
        /// Stringified probe error.
        detail: String,
    },
    /// Dimensions were readable and under the cap, but the actual decode
    /// failed (truncated stream, corrupt payload past the header).
    DecodeFailed {
        /// Stringified decode error.
        detail: String,
    },
}

impl std::fmt::Display for DecodeRgbaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PixelCapExceeded { pixels, max_pixels } => write!(
                f,
                "image dimensions {pixels} pixels exceed the decoded-pixel cap ({max_pixels})"
            ),
            Self::DimensionsUnreadable { detail } => write!(f, "image probe failed: {detail}"),
            Self::DecodeFailed { detail } => write!(f, "image decode failed: {detail}"),
        }
    }
}

impl std::error::Error for DecodeRgbaError {}

/// Decode an encoded image to RGBA, refusing canvases above `max_pixels`.
///
/// The dimension probe reads only the format header (PNG's IHDR, JPEG's
/// SOF, …) via `image::ImageReader::into_dimensions`, so it stays bounded
/// even when the encoded payload itself is multiple MB. Only when the
/// advertised `width × height` fits under `max_pixels` does the actual
/// `decode` + RGBA conversion run.
pub fn decode_rgba_with_pixel_cap(
    bytes: &[u8],
    max_pixels: u64,
) -> Result<RgbaImage, DecodeRgbaError> {
    let (width, height) = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|err| DecodeRgbaError::DimensionsUnreadable {
            detail: err.to_string(),
        })?
        .into_dimensions()
        .map_err(|err| DecodeRgbaError::DimensionsUnreadable {
            detail: err.to_string(),
        })?;
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if pixels > max_pixels {
        return Err(DecodeRgbaError::PixelCapExceeded { pixels, max_pixels });
    }
    let decoded = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|err| DecodeRgbaError::DecodeFailed {
            detail: err.to_string(),
        })?
        .decode()
        .map_err(|err| DecodeRgbaError::DecodeFailed {
            detail: err.to_string(),
        })?;
    Ok(decoded.to_rgba8())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_real_png(width: u32, height: u32) -> Vec<u8> {
        let mut png = Vec::new();
        let img = RgbaImage::new(width, height);
        img.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("encode small PNG");
        png
    }

    #[test]
    fn decodes_an_image_under_the_cap() {
        let png = encode_real_png(8, 8);
        let rgba = decode_rgba_with_pixel_cap(&png, 64).expect("8x8 fits a 64-pixel cap exactly");
        assert_eq!(rgba.dimensions(), (8, 8));
    }

    #[test]
    fn rejects_a_canvas_above_the_cap_before_decoding() {
        let png = encode_real_png(8, 8);
        let err = decode_rgba_with_pixel_cap(&png, 63).expect_err("64 pixels exceed a 63 cap");
        assert!(
            matches!(
                err,
                DecodeRgbaError::PixelCapExceeded {
                    pixels: 64,
                    max_pixels: 63,
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn flags_unreadable_dimensions_without_decoding() {
        let err = decode_rgba_with_pixel_cap(b"definitely not an image", u64::MAX)
            .expect_err("unrecognisable bytes cannot yield dimensions");
        assert!(
            matches!(err, DecodeRgbaError::DimensionsUnreadable { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn flags_a_decode_failure_after_a_readable_header() {
        // Truncate a real PNG right after its IDAT chunk header: the
        // dimension probe (which stops once it has located IHDR/IDAT)
        // still succeeds, but the actual decode runs out of stream.
        let png = encode_real_png(8, 8);
        let idat = png
            .windows(4)
            .position(|w| w == b"IDAT")
            .expect("PNG has an IDAT chunk");
        let truncated = &png[..idat + 4];

        let err = decode_rgba_with_pixel_cap(truncated, u64::MAX)
            .expect_err("a truncated IDAT stream cannot decode");
        assert!(
            matches!(err, DecodeRgbaError::DecodeFailed { .. }),
            "got {err:?}"
        );
    }
}
