use std::borrow::Cow;
use std::io::Cursor;
use std::sync::{Arc, Mutex};

use arboard::{Clipboard, ImageData};
use async_trait::async_trait;
use image::{ImageFormat, ImageReader};
use nagori_core::{
    AppError, ClipboardContent, ClipboardData, ClipboardEntry, ClipboardRepresentation,
    ClipboardSequence, ClipboardSnapshot, Result,
};
use nagori_platform::{CapturedSnapshot, ClipboardReader, ClipboardWriter};
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
        tokio::task::spawn_blocking(move || -> Result<ClipboardSnapshot> {
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
        tokio::task::spawn_blocking(|| {
            ClipboardSequence::native(i64::from(native_sequence_number()))
        })
        .await
        .map_err(|err| AppError::Platform(err.to_string()))
    }

    async fn current_snapshot_with_max(&self, max_bytes: usize) -> Result<CapturedSnapshot> {
        let clipboard = self.clipboard.clone();
        tokio::task::spawn_blocking(move || capture_snapshot(&clipboard, Some(max_bytes)))
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
        tokio::task::spawn_blocking(move || -> Result<()> {
            clipboard
                .lock()
                .map_err(|err| lock_err(&err))?
                .set_text(owned)
                .map_err(|err| platform_err(&err))
        })
        .await
        .map_err(|err| AppError::Platform(err.to_string()))?
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
        tokio::task::spawn_blocking(move || -> Result<()> {
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
        // the format. The decode runs on the blocking pool because
        // `image::ImageReader::decode` is CPU-bound.
        let clipboard = self.clipboard.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let rgba = ImageReader::new(Cursor::new(&bytes))
                .with_guessed_format()
                .map_err(|err| AppError::Platform(format!("image probe failed: {err}")))?
                .decode()
                .map_err(|err| AppError::Platform(format!("image decode failed: {err}")))?
                .to_rgba8();
            let (width, height) = rgba.dimensions();
            let image_data = ImageData {
                width: width as usize,
                height: height as usize,
                bytes: Cow::Owned(rgba.into_raw()),
            };
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
        // arboard's `get_image` opens its own `OpenClipboard` session and pulls
        // the raw `CF_DIBV5` (falling back to `CF_DIB`) bytes into a freshly
        // allocated RGBA buffer. Encode to PNG so the rest of the pipeline
        // (storage, search snippets, IPC, copy-back) can treat Windows
        // captures the same way macOS publishes `image/png` straight off the
        // pasteboard. Format unavailability is the common case and surfaces
        // as `ContentNotAvailable`, which we silently skip — only true Win32
        // failures bubble up as `AppError::Platform`.
        let image = match guard.get_image() {
            Ok(img) => Some(img),
            Err(arboard::Error::ContentNotAvailable) => None,
            Err(err) => return Err(platform_err(&err)),
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
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::{char, mem, slice};

    use windows_sys::Win32::Foundation::{GlobalFree, TRUE};
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
        OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
    };
    use windows_sys::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT};
    use windows_sys::Win32::UI::Shell::{DROPFILES, DragQueryFileW};

    use nagori_core::{AppError, Result};

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
            // CF_DIBV5 is the canonical format apps publish today; CF_DIB
            // remains for compatibility with older sources. Prefer V5 first
            // so the size reported is the one arboard will actually pull
            // when it decodes the image. The raw DIB blob is uncompressed
            // (~width*height*4 bytes), which is an over-estimate compared
            // to the PNG we eventually push into storage, but that bias is
            // the safe direction: we reject before allocating an RGBA copy
            // rather than learning the payload is huge only after decode.
            if let Some(image_bytes) = image_format_size() {
                observed = observed.saturating_add(image_bytes);
                if observed > max_bytes {
                    return Some(observed);
                }
            }
            None
        }
    }

    unsafe fn image_format_size() -> Option<usize> {
        // arboard prefers a registered `"PNG"` format if it is offered
        // (then `CF_DIBV5`, then `CF_DIB`). If we only probed the standard
        // bitmap formats, a publisher that registers PNG without also
        // offering DIB would slip past the size guard and force the
        // capture path to read a multi-MB blob before failing further
        // along. Mirror arboard's lookup order so the probe matches the
        // bytes the reader is actually about to materialise.
        unsafe {
            if let Some(png_id) = png_format_id()
                && IsClipboardFormatAvailable(png_id) != 0
                && let Some(bytes) = global_data_size(png_id)
            {
                return Some(bytes);
            }
            for format in [CF_DIBV5, CF_DIB] {
                if IsClipboardFormatAvailable(u32::from(format)) != 0
                    && let Some(bytes) = global_data_size(u32::from(format))
                {
                    return Some(bytes);
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
        // `DROPFILES.pFiles` is `u32`. The struct's layout is fixed at
        // compile time (~20 bytes), so the truncation can never happen,
        // but expressing it as `try_from` keeps the conversion explicit
        // instead of paving over it with a cast-allow attribute.
        let header_size_u32 = u32::try_from(header_size)
            .map_err(|_| AppError::Platform("DROPFILES header size exceeds u32".to_owned()))?;
        let payload_bytes = wide_buffer.len().saturating_mul(mem::size_of::<u16>());
        let total_bytes = header_size.saturating_add(payload_bytes);

        // SAFETY: every Win32 call below is paired with its release.
        // `GlobalAlloc` returns null on failure and we surface that as
        // a platform error. On every error path between the allocation
        // and a successful `SetClipboardData` we call `GlobalFree`;
        // after `SetClipboardData` succeeds, the OS owns the handle.
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
            // Copy the wide-char path buffer as raw bytes so we don't
            // tell clippy we're hand-aligning a `*mut u8` up to `*mut u16`.
            // `GlobalAlloc` returns at least 8-byte alignment and the
            // DROPFILES header is 4-byte aligned, so the destination is
            // 2-byte aligned in practice — copying as bytes is just
            // hygienic.
            std::ptr::copy_nonoverlapping(
                wide_buffer.as_ptr().cast::<u8>(),
                locked.cast::<u8>().add(header_size),
                payload_bytes,
            );
            let _ = GlobalUnlock(handle);

            if OpenClipboard(std::ptr::null_mut()) == 0 {
                GlobalFree(handle);
                return Err(AppError::Platform(
                    "OpenClipboard failed for CF_HDROP write".to_owned(),
                ));
            }
            let _guard = ClipboardGuard;
            if EmptyClipboard() == 0 {
                GlobalFree(handle);
                return Err(AppError::Platform(
                    "EmptyClipboard failed for CF_HDROP write".to_owned(),
                ));
            }
            if SetClipboardData(u32::from(CF_HDROP), handle).is_null() {
                // SetClipboardData failed → ownership is still ours, free it.
                GlobalFree(handle);
                return Err(AppError::Platform(
                    "SetClipboardData(CF_HDROP) failed".to_owned(),
                ));
            }
            // From here the OS owns the handle; do not free.
            Ok(())
        }
    }
}

fn platform_err(err: &arboard::Error) -> AppError {
    AppError::Platform(err.to_string())
}

fn lock_err<T>(err: &std::sync::PoisonError<T>) -> AppError {
    AppError::Platform(err.to_string())
}
