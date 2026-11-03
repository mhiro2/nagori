//! On-device text recognition over the Vision framework.
//!
//! The Swift side (`nagori_apple_recognize_text_c`) runs a
//! `VNRecognizeTextRequest` synchronously and hands back one tab-separated
//! line per recognized text line; [`parse_recognized_lines`] turns that frame
//! into the core [`RecognizedText`]. The parser is plain Rust so every
//! platform's tests cover it; only the FFI call is macOS-gated.

use nagori_core::{RecognizedLine, RecognizedText, TextBounds};

/// Parse the Swift bridge's `x\ty\twidth\theight\ttext` lines.
///
/// Lines that do not carry four finite coordinates and non-empty text are
/// skipped rather than failing the whole recognition, and coordinates are
/// clamped to the image so a box never points outside it.
#[must_use]
pub fn parse_recognized_lines(frame: &str) -> RecognizedText {
    let lines = frame
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(5, '\t');
            let mut coord = || {
                fields
                    .next()?
                    .parse::<f32>()
                    .ok()
                    .filter(|value| value.is_finite())
                    .map(|value| value.clamp(0.0, 1.0))
            };
            let bounds = TextBounds {
                x: coord()?,
                y: coord()?,
                width: coord()?,
                height: coord()?,
            };
            let text = fields.next()?.trim();
            (!text.is_empty()).then(|| RecognizedLine {
                text: text.to_owned(),
                bounds,
            })
        })
        .collect();
    RecognizedText { lines }
}

#[cfg(target_os = "macos")]
pub use apple::AppleTextRecognizer;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod apple {
    use std::ffi::c_void;

    use async_trait::async_trait;
    use nagori_core::{AppError, Bytes, RecognizedText, Result, TextRecognizer};

    use super::parse_recognized_lines;

    unsafe extern "C" {
        fn nagori_apple_recognize_text_c(
            data_ptr: *const u8,
            data_len: usize,
            ctx: *mut c_void,
            on_result: extern "C" fn(*mut c_void, *const u8, usize),
        ) -> i32;
    }

    /// Copies the frame Swift hands over into the `Vec<u8>` behind `ctx`.
    extern "C" fn collect_frame(ctx: *mut c_void, ptr: *const u8, len: usize) {
        if ctx.is_null() || ptr.is_null() {
            return;
        }
        // SAFETY: `ctx` is the `&mut Vec<u8>` `recognize_blocking` passed in,
        // live for the whole synchronous call; `ptr`/`len` describe a buffer
        // Swift keeps valid for the duration of this callback.
        let (out, bytes) = unsafe {
            (
                &mut *ctx.cast::<Vec<u8>>(),
                std::slice::from_raw_parts(ptr, len),
            )
        };
        out.extend_from_slice(bytes);
    }

    fn recognize_blocking(image: &[u8]) -> Result<RecognizedText> {
        let mut frame: Vec<u8> = Vec::new();
        let ctx = std::ptr::from_mut(&mut frame).cast::<c_void>();
        // SAFETY: the signature matches the `@_cdecl` export; Swift reads
        // `image` and calls `collect_frame` only during this call, while both
        // `image` and `frame` are live.
        let code = unsafe {
            nagori_apple_recognize_text_c(image.as_ptr(), image.len(), ctx, collect_frame)
        };
        if code != 0 {
            return Err(AppError::Platform(
                "text recognition could not read this image".to_owned(),
            ));
        }
        Ok(parse_recognized_lines(&String::from_utf8_lossy(&frame)))
    }

    /// [`TextRecognizer`] backed by the Vision framework. Recognition is
    /// CPU/ANE-bound and synchronous, so it runs on the blocking pool.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct AppleTextRecognizer;

    impl AppleTextRecognizer {
        #[must_use]
        pub const fn new() -> Self {
            Self
        }
    }

    #[async_trait]
    impl TextRecognizer for AppleTextRecognizer {
        async fn recognize(&self, image: Bytes) -> Result<RecognizedText> {
            tokio::task::spawn_blocking(move || recognize_blocking(&image))
                .await
                .map_err(|err| AppError::Platform(format!("text recognition task failed: {err}")))?
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lines_and_skips_malformed_ones() {
        let frame = "0.1\t0.2\t0.5\t0.05\terror: connection refused\n\
                     bad line\n\
                     0.1\tNaN\t0.5\t0.05\tnan box\n\
                     0.1\t0.3\t0.5\t0.05\t   \n\
                     -0.2\t0.9\t1.5\t0.05\t接続できません\n";
        let parsed = parse_recognized_lines(frame);
        assert_eq!(parsed.lines.len(), 2);
        assert_eq!(parsed.lines[0].text, "error: connection refused");
        assert!((parsed.lines[0].bounds.y - 0.2).abs() < f32::EPSILON);
        assert_eq!(parsed.lines[1].text, "接続できません");
        assert!(parsed.lines[1].bounds.x.abs() < f32::EPSILON);
        assert!((parsed.lines[1].bounds.width - 1.0).abs() < f32::EPSILON);
    }

    /// Live Vision run against a real image: set `NAGORI_OCR_SAMPLE` to a PNG
    /// holding `error: connection refused` and run with `--ignored`.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "needs NAGORI_OCR_SAMPLE pointing at a sample screenshot"]
    async fn live_recognizes_text_in_a_screenshot() {
        use nagori_core::TextRecognizer;

        let path = std::env::var("NAGORI_OCR_SAMPLE").expect("NAGORI_OCR_SAMPLE");
        let bytes = std::fs::read(path).expect("sample image");
        let recognized = AppleTextRecognizer::new()
            .recognize(bytes.into())
            .await
            .expect("recognize");
        let text = recognized.text();
        eprintln!("{text}");
        assert!(text.contains("connection refused"), "{text}");
        assert!(recognized.lines.iter().all(|line| {
            let b = line.bounds;
            b.x + b.width <= 1.0 + f32::EPSILON && b.y + b.height <= 1.0 + f32::EPSILON
        }));
    }

    #[test]
    fn keeps_tabs_out_of_the_coordinates() {
        let parsed = parse_recognized_lines("0\t0\t1\t1\ta\tb");
        assert_eq!(parsed.lines[0].text, "a\tb");
    }
}
