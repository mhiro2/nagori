use std::sync::{Arc, Mutex};

use arboard::Clipboard;
use async_trait::async_trait;
use nagori_core::{
    AppError, ClipboardContent, ClipboardData, ClipboardEntry, ClipboardRepresentation,
    ClipboardSequence, ClipboardSnapshot, Result,
};
use nagori_platform::{ClipboardReader, ClipboardWriter};
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
            const MAX_RETRIES: usize = 3;
            let mut attempt = 0;
            loop {
                attempt += 1;
                let before = native_sequence_number();
                let mut guard = clipboard.lock().map_err(|err| lock_err(&err))?;
                let plain = match guard.get_text() {
                    Ok(text) => Some(text),
                    Err(arboard::Error::ContentNotAvailable) => None,
                    Err(err) => return Err(platform_err(&err)),
                };
                // Drop the arboard guard before the second Win32 read so
                // we don't hold it across the CF_HDROP OpenClipboard
                // call; the sequence-stability check is what protects
                // us against a write landing in between.
                drop(guard);

                let mut representations = Vec::new();

                #[cfg(windows)]
                if let Some(files) = win::read_file_list() {
                    representations.push(ClipboardRepresentation {
                        mime_type: "text/uri-list".to_owned(),
                        data: ClipboardData::FilePaths(files),
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
                    return Ok(ClipboardSnapshot {
                        sequence: ClipboardSequence::native(i64::from(after)),
                        captured_at: OffsetDateTime::now_utc(),
                        source: None,
                        representations,
                    });
                }
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
}

#[async_trait]
impl ClipboardWriter for WindowsClipboard {
    async fn write_entry(&self, entry: &ClipboardEntry) -> Result<()> {
        if let ClipboardContent::Image(_) = &entry.content {
            return Err(AppError::Unsupported(
                "image clipboard writes are not implemented on Windows yet".to_owned(),
            ));
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
    use std::os::windows::ffi::OsStringExt;

    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    };
    use windows_sys::Win32::System::Ole::CF_HDROP;
    use windows_sys::Win32::UI::Shell::DragQueryFileW;

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
}

fn platform_err(err: &arboard::Error) -> AppError {
    AppError::Platform(err.to_string())
}

fn lock_err<T>(err: &std::sync::PoisonError<T>) -> AppError {
    AppError::Platform(err.to_string())
}
