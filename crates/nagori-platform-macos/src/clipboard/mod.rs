use std::sync::{Arc, Mutex};

use arboard::Clipboard;
use nagori_core::{ClipboardSequence, Result};
use nagori_platform::platform_err;
#[cfg(target_os = "macos")]
use objc2_app_kit::NSPasteboard;

#[cfg(target_os = "macos")]
const MAX_FILE_URL_ITEMS: usize = 4096;

/// Hard ceiling on a single pasteboard image representation copied into the
/// daemon's heap.
///
/// The pasteboard owner controls `NSData.length`, and TIFF is deliberately
/// exempt from the `oversized_payload` pre-filter (it is normalised to PNG
/// *before* the entry-size gate so an uncompressed screenshot does not look
/// oversized) — so without this check a malicious owner advertising a
/// multi-GB TIFF would land its full payload in `ns_data_to_vec`. PNG is
/// covered by the pre-filter on the bounded path but not on the unbounded
/// `current_snapshot` path, so both branches check the constant-time
/// `length()` before copying. Mirrors the Linux adapter's
/// `INTERNAL_BODY_CEILING_BYTES` defence-in-depth ceiling: 256 MiB is far
/// above any realistic screenshot or copied image.
#[cfg(target_os = "macos")]
const MAX_IMAGE_REP_BYTES: usize = 256 * 1024 * 1024;

/// UTI mapped to `image/jpeg` for pasteboard publishing.
///
/// `NSPasteboardType` is a `NSString` newtype, so MIMEs that lack a static
/// `NSPasteboardTypePNG`-style constant are still publishable by building a
/// fresh `NSString` from the canonical Uniform Type Identifier. Apps that
/// register `public.jpeg` / `com.compuserve.gif` / `org.webmproject.webp`
/// in their `NSServices` registration (Preview, `TextEdit`, Mail, most
/// browsers) accept these the same way they accept the bundled constants.
#[cfg(target_os = "macos")]
const UTI_JPEG: &str = "public.jpeg";
#[cfg(target_os = "macos")]
const UTI_GIF: &str = "com.compuserve.gif";
#[cfg(target_os = "macos")]
const UTI_WEBP: &str = "org.webmproject.webp";

/// nspasteboard.org marker an owner sets to declare "do not record this in
/// history". A password manager flags a copied secret with
/// [`MARKER_CONCEALED`]; an app that puts a throwaway value on the clipboard
/// flags it with [`MARKER_TRANSIENT`]. We treat both as a hard skip — see
/// [`pasteboard_exclusion`].
#[cfg(target_os = "macos")]
const MARKER_CONCEALED: &str = "org.nspasteboard.ConcealedType";
#[cfg(target_os = "macos")]
const MARKER_TRANSIENT: &str = "org.nspasteboard.TransientType";

/// macOS clipboard adapter.
///
/// The `Arc<Mutex<Clipboard>>` is **not** there for arboard's internal
/// thread-safety alone — `Clipboard::get_text` / `set_text` already take
/// `&mut self`, so exclusive access would be required regardless. The lock
/// extends across `get_text` + `collect_macos_extras` (and the `dataForType`
/// loads in `current_snapshot_with_max`) so a concurrent `write_image_bytes`
/// cannot slip its `clearContents`/`setData` pair between the two and stitch
/// a torn snapshot.
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

#[cfg(target_os = "macos")]
mod file_url;
mod read;
mod transcode;
mod write;

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
const fn pasteboard_sequence() -> ClipboardSequence {
    ClipboardSequence::unsupported()
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests;
