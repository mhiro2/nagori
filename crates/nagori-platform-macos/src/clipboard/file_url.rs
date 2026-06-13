#[cfg(target_os = "macos")]
use nagori_platform::ClipboardExclusionKind;
#[cfg(target_os = "macos")]
use objc2_app_kit::{
    NSPasteboard, NSPasteboardItem, NSPasteboardTypeFileURL, NSPasteboardTypeHTML,
    NSPasteboardTypePNG, NSPasteboardTypeRTF, NSPasteboardTypeString,
};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSArray, NSString, NSURL};

use super::{MARKER_CONCEALED, MARKER_TRANSIENT, MAX_FILE_URL_ITEMS};

#[cfg(target_os = "macos")]
pub(super) enum FileUrlPaths {
    Captured(Vec<String>),
    Oversized(usize),
}

#[cfg(target_os = "macos")]
pub(super) fn collect_file_url_paths(
    items: &NSArray<NSPasteboardItem>,
    max_bytes: Option<usize>,
) -> FileUrlPaths {
    let mut paths = Vec::new();
    let mut observed_bytes = 0_usize;
    let mut file_url_count = 0_usize;

    for item in items {
        // SAFETY: `NSPasteboardTypeFileURL` is a static AppKit pasteboard type
        // constant with framework lifetime.
        let Some(string) = item.stringForType(unsafe { NSPasteboardTypeFileURL }) else {
            continue;
        };
        file_url_count = file_url_count.saturating_add(1);
        observed_bytes = observed_bytes.saturating_add(string.len());

        // Cap the item count unconditionally — including the unbounded
        // (`max_bytes == None`) `current_snapshot` path — so a pasteboard
        // advertising millions of file URLs cannot grow `paths` without bound.
        // Mirrors Windows' unconditional `MAX_PATHS` cap; the byte-budget check
        // below still only runs when a caller passes a limit.
        if file_url_count > MAX_FILE_URL_ITEMS {
            let observed = max_bytes.map_or(observed_bytes, |limit| {
                observed_bytes.max(limit_exceeded_bytes(limit))
            });
            return FileUrlPaths::Oversized(observed);
        }
        if let Some(limit) = max_bytes
            && observed_bytes > limit
        {
            return FileUrlPaths::Oversized(observed_bytes);
        }

        // Finder frequently lands a *file reference URL* on the pasteboard;
        // resolve it to a path-based URL before decoding so the palette shows
        // the real path instead of the `/.file/id=…` handle. An unresolvable
        // reference URL is dropped so the handle never leaks to the palette.
        let raw = match resolve_file_url(&string) {
            FileUrlResolution::Resolved(resolved) => resolved,
            FileUrlResolution::Raw => string.to_string(),
            FileUrlResolution::Drop => continue,
        };
        if let Some(path) = file_url_to_path(&raw) {
            paths.push(path);
        }
    }

    FileUrlPaths::Captured(paths)
}

/// Detect an owner-declared "do not record this" marker on the general
/// pasteboard, returning the [`ClipboardExclusionKind`] when present.
///
/// `availableTypeFromArray` returns the first candidate type the pasteboard
/// offers, so listing `Concealed` before `Transient` fixes the priority the
/// capture loop relies on: when an owner sets both markers, the concealed
/// (secret) signal wins. Only the marker's *presence* matters — we never ask
/// for its data — so the secret body is never pulled into our address space.
/// This runs before the `oversized_payload` probe and the `get_text` body
/// read in [`capture_attempt`], mirroring how the capture loop skips a secure
/// focus before reading the clipboard.
#[cfg(target_os = "macos")]
pub(super) fn pasteboard_exclusion() -> Option<ClipboardExclusionKind> {
    // Same autoreleasepool discipline as `oversized_payload` /
    // `pasteboard_sequence`: drain the AppKit temporaries (`+generalPasteboard`,
    // the candidate NSStrings, the returned type) on every call so the
    // blocking-pool thread does not accumulate them across polls.
    objc2::rc::autoreleasepool(|_pool| exclusion_for(&NSPasteboard::generalPasteboard()))
}

/// Marker test for a specific pasteboard. Split out from
/// [`pasteboard_exclusion`] so a unit test can exercise the detection and
/// `Concealed`-priority logic against an isolated `pasteboardWithUniqueName`
/// rather than clobbering the shared general pasteboard.
///
/// `availableTypeFromArray` returns the first candidate the receiver offers,
/// so listing `Concealed` before `Transient` makes a concealed secret win
/// when an owner sets both. It is a presence test on the receiver's declared
/// types and is unrelated to the `NSPasteboard` Filter Services that convert
/// between known UTIs, so it never spuriously reports an opaque marker type.
#[cfg(target_os = "macos")]
pub(super) fn exclusion_for(pb: &NSPasteboard) -> Option<ClipboardExclusionKind> {
    let candidates = NSArray::from_retained_slice(&[
        NSString::from_str(MARKER_CONCEALED),
        NSString::from_str(MARKER_TRANSIENT),
    ]);
    let present = pb.availableTypeFromArray(&candidates)?;
    let ty = present.to_string();
    if ty == MARKER_TRANSIENT {
        Some(ClipboardExclusionKind::Transient)
    } else {
        // `availableTypeFromArray` only returns a type we listed, and the
        // array lists `Concealed` first, so anything that is not the
        // transient marker is the concealed one.
        Some(ClipboardExclusionKind::Concealed)
    }
}

/// Probe `NSPasteboard` for any single representation whose byte length
/// exceeds `max_bytes`, returning the observed length on first hit.
///
/// `NSData::length` is constant-time and avoids the `to_vec()` copy that
/// `ns_data_to_vec` would otherwise perform. `NSString::len` reports exact
/// UTF-8 byte length without materialising a Rust `String`, so text and file
/// URL probes can be compared directly against `max_bytes`.
#[cfg(target_os = "macos")]
pub(super) fn oversized_payload(max_bytes: usize) -> Option<usize> {
    // Same rationale as `collect_macos_extras`: drain the AppKit
    // autoreleased temporaries on every call so the blocking-pool thread
    // does not retain pasteboard data past return.
    objc2::rc::autoreleasepool(|_pool| {
        // SAFETY: AppKit FFI on the shared pasteboard. All getters return
        // optional retained references and we only read lengths on the
        // returned objects, which has no observable side effects and does
        // not require holding the pasteboard lock beyond the call itself.
        unsafe {
            let pb = NSPasteboard::generalPasteboard();

            if let Some(items) = pb.pasteboardItems()
                && let Some(observed) = oversized_file_urls(&items, max_bytes)
            {
                return Some(observed);
            }
            if let Some(data) = pb.dataForType(NSPasteboardTypePNG)
                && data.length() > max_bytes
            {
                return Some(data.length());
            }
            if let Some(string) = pb.stringForType(NSPasteboardTypeHTML)
                && string.len() > max_bytes
            {
                return Some(string.len());
            }
            if let Some(string) = pb.stringForType(NSPasteboardTypeRTF)
                && string.len() > max_bytes
            {
                return Some(string.len());
            }
            if let Some(string) = pb.stringForType(NSPasteboardTypeString)
                && string.len() > max_bytes
            {
                return Some(string.len());
            }
        }
        None
    })
}

#[cfg(target_os = "macos")]
pub(super) fn oversized_file_urls(
    items: &NSArray<NSPasteboardItem>,
    max_bytes: usize,
) -> Option<usize> {
    let mut observed_bytes = 0_usize;
    let mut file_url_count = 0_usize;

    for item in items {
        // SAFETY: `NSPasteboardTypeFileURL` is a static AppKit pasteboard type
        // constant with framework lifetime.
        let Some(string) = item.stringForType(unsafe { NSPasteboardTypeFileURL }) else {
            continue;
        };
        file_url_count = file_url_count.saturating_add(1);
        observed_bytes = observed_bytes.saturating_add(string.len());

        if file_url_count > MAX_FILE_URL_ITEMS {
            return Some(observed_bytes.max(limit_exceeded_bytes(max_bytes)));
        }
        if observed_bytes > max_bytes {
            return Some(observed_bytes);
        }
    }

    None
}

#[cfg(target_os = "macos")]
const fn limit_exceeded_bytes(limit: usize) -> usize {
    limit.saturating_add(1)
}

#[cfg(target_os = "macos")]
pub(super) fn ns_data_to_vec(data: &objc2_foundation::NSData) -> Option<Vec<u8>> {
    if data.is_empty() {
        return None;
    }
    Some(data.to_vec())
}

/// Outcome of inspecting a pasteboard file URL string.
#[cfg(target_os = "macos")]
enum FileUrlResolution {
    /// A file reference URL resolved to this path-based `file://` URL string.
    Resolved(String),
    /// Not a reference URL (or unparseable as an `NSURL`) — decode the raw
    /// pasteboard string directly, preserving prior behaviour for plain path
    /// URLs.
    Raw,
    /// A file reference URL whose target no longer resolves (deleted file,
    /// unmounted volume). There is no real path to show, so the entry is
    /// dropped rather than leaking the `/.file/id=…` handle.
    Drop,
}

/// Resolve a pasteboard file URL string to a path-based `file://` URL.
///
/// Finder copies often publish a *file reference URL*
/// (`file:///.file/id=6571367.5488049`, an inode-based handle) rather than a
/// path URL. The `url` crate can't resolve that form, so [`file_url_to_path`]
/// would surface the literal `/.file/id=…` to the palette even though pasting
/// the entry elsewhere yields the real path. `NSURL::filePathURL` resolves a
/// reference URL to its path-based equivalent; only file reference URLs are
/// routed through it so an unresolvable handle is dropped while plain path
/// URLs keep their existing raw-string decode.
#[cfg(target_os = "macos")]
fn resolve_file_url(raw: &NSString) -> FileUrlResolution {
    let Some(url) = NSURL::URLWithString(raw) else {
        return FileUrlResolution::Raw;
    };
    if !url.isFileReferenceURL() {
        return FileUrlResolution::Raw;
    }
    match url
        .filePathURL()
        .and_then(|resolved| resolved.absoluteString())
    {
        Some(abs) => FileUrlResolution::Resolved(abs.to_string()),
        None => FileUrlResolution::Drop,
    }
}

#[cfg(target_os = "macos")]
fn file_url_to_path(raw: &str) -> Option<String> {
    let parsed = url::Url::parse(raw).ok()?;
    if parsed.scheme() != "file" {
        return None;
    }
    parsed
        .to_file_path()
        .ok()
        .and_then(|path| path.into_os_string().into_string().ok())
}
