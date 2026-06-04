use async_trait::async_trait;
use nagori_core::Result;
#[cfg(windows)]
use nagori_core::SourceApp;
use nagori_platform::{FrontmostApp, RestoreTarget, WindowBehavior};

/// Windows frontmost-app probe.
///
/// Uses `GetForegroundWindow` → `GetWindowThreadProcessId` →
/// `QueryFullProcessImageNameW` to extract the executable path of the
/// process that owns the foreground window. `GetWindowTextW` provides the
/// window title used for source-attribution displays. Mirrors the macOS
/// adapter's contract: returns `Ok(None)` on failure rather than `Err`,
/// so the capture loop can proceed without source metadata.
///
/// `frontmost_focused_is_secure` keeps the trait default (`Ok(false)`) —
/// see the doc on `WindowBehavior::frontmost_focused_is_secure`. The
/// current Win32-based adapter does **not** wire a secure-text probe:
/// Win32 alone does not surface another process's focused control's
/// secure flag (we'd need to walk across a security boundary), and we
/// have not adopted the UI Automation alternative
/// (`IUIAutomation::GetFocusedElement` + `IsPasswordProperty`) yet.
/// Until that lands, the password-input guard relies on the
/// `SensitivityClassifier` content detectors (PEM blocks, JWTs) and the
/// password-manager source-app denylist seeded by `frontmost_app()`.
#[derive(Debug, Default)]
pub struct WindowsWindowBehavior;

/// Upper bound on a blocking Win32 window op (`GetForegroundWindow` probing,
/// `SetForegroundWindow` focus restore). A healthy call answers in a few ms;
/// bounding the `spawn_blocking` hop keeps a wedged `USER32` lock from leaving
/// the focus-restore step — which runs just before the synthesised Ctrl+V —
/// pending forever.
const WINDOW_OP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

impl WindowsWindowBehavior {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Synchronous variant of `frontmost_app` for callers that are
    /// already on a thread where blocking on Win32 is acceptable (e.g. a
    /// Tauri global-shortcut callback). Mirrors `MacosWindowBehavior`.
    ///
    /// Not `const` because on Windows the body performs FFI calls; on
    /// non-Windows hosts (cargo check, tests) it folds to a no-op which
    /// clippy keeps flagging as const-eligible — the
    /// `missing_const_for_fn` allow suppresses that off-target false
    /// positive without weakening the on-target signature.
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn frontmost_app_blocking() -> Option<FrontmostApp> {
        frontmost_app_sync()
    }

    /// Capture a [`RestoreTarget`] snapshot at palette-open time. Unlike
    /// `frontmost_app_blocking`, this also stamps the HWND into
    /// `native_handle` so `activate_restore_target` can later call
    /// `SetForegroundWindow` against the *original* window — necessary
    /// because Windows has no bundle id and several top-level windows in
    /// the same executable would otherwise be indistinguishable.
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn capture_restore_target_blocking() -> Option<RestoreTarget> {
        capture_restore_target_sync()
    }
}

#[async_trait]
impl WindowBehavior for WindowsWindowBehavior {
    async fn frontmost_app(&self) -> Result<Option<FrontmostApp>> {
        nagori_platform::run_blocking_with_timeout(
            "frontmost_app",
            WINDOW_OP_TIMEOUT,
            frontmost_app_sync,
        )
        .await
        .map_err(|err| nagori_core::AppError::Platform(err.to_string()))
    }

    async fn show_palette(&self) -> Result<()> {
        Ok(())
    }

    async fn hide_palette(&self) -> Result<()> {
        Ok(())
    }

    async fn activate_restore_target(&self, target: &RestoreTarget) -> Result<()> {
        let Some(handle) = target.native_handle else {
            return Ok(());
        };
        let snapshot_pid = target.snapshot_pid;
        let snapshot_exe = target.source.executable_path.clone();
        // Focus restore runs after the palette hides and before the
        // synthesised Ctrl+V. Bound it so a wedged USER32 lock can't hang the
        // paste flow — on timeout we surface a platform error and the desktop
        // aborts the paste rather than spraying Ctrl+V into whatever window
        // kept focus.
        nagori_platform::run_blocking_with_timeout(
            "activate_restore_target",
            WINDOW_OP_TIMEOUT,
            move || activate_hwnd_sync(handle, snapshot_pid, snapshot_exe.as_deref()),
        )
        .await
        .map_err(|err| nagori_core::AppError::Platform(err.to_string()))?
        .map_err(nagori_core::AppError::Platform)
    }
}

/// Derive a process display name from its executable path via the file
/// stem (`C:/Program Files/App/app.exe` → `app`).
///
/// Shared by `frontmost_app_sync` and `capture_restore_target_sync`.
/// Gated to `any(windows, test)`: the only non-test callers are the
/// Windows FFI paths, so an unconditional definition reads as dead code
/// on other hosts, while `test` keeps it reachable from the unit test on
/// every host. Separator handling follows `std::path`, so backslash paths
/// only split on Windows — the Windows callers feed it native paths.
#[cfg(any(windows, test))]
fn app_name_from_exe(executable_path: Option<&str>) -> Option<String> {
    executable_path.and_then(|p| {
        std::path::Path::new(p)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_owned)
    })
}

#[cfg(windows)]
fn frontmost_app_sync() -> Option<FrontmostApp> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };

    // SAFETY: GetForegroundWindow has no parameters and returns a HWND;
    // GetWindowThreadProcessId writes the owning PID through the out
    // pointer to our stack-owned `u32`.
    let (executable_path, window_title) = unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let mut pid: u32 = 0;
        if GetWindowThreadProcessId(hwnd, &raw mut pid) == 0 {
            return None;
        }
        (query_process_image_path(pid), query_window_title(hwnd))
    };

    let name = app_name_from_exe(executable_path.as_deref());

    Some(FrontmostApp {
        source: SourceApp {
            bundle_id: None,
            name,
            executable_path,
        },
        window_title,
    })
}

#[cfg(not(windows))]
const fn frontmost_app_sync() -> Option<FrontmostApp> {
    None
}

#[cfg(windows)]
fn capture_restore_target_sync() -> Option<RestoreTarget> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };

    // SAFETY: GetForegroundWindow has no parameters and returns a HWND;
    // GetWindowThreadProcessId writes the owning PID through the out
    // pointer to our stack-owned `u32`.
    let (hwnd, pid, executable_path, window_title) = unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let mut pid: u32 = 0;
        if GetWindowThreadProcessId(hwnd, &raw mut pid) == 0 {
            return None;
        }
        (
            hwnd,
            pid,
            query_process_image_path(pid),
            query_window_title(hwnd),
        )
    };

    let name = app_name_from_exe(executable_path.as_deref());

    // HWND is a pointer-sized opaque on both 32- and 64-bit Windows. We
    // round-trip via `usize` so the cast is exact regardless of pointer
    // width — `as u64` of a *mut c_void on 32-bit silently sign-extends
    // a hostile signed cast under `clippy::ptr_as_ptr`.
    #[allow(clippy::cast_possible_truncation)] // hwnd fits in usize by definition
    let native_handle = Some(hwnd as usize as u64);
    let _ = window_title;

    Some(RestoreTarget {
        source: SourceApp {
            bundle_id: None,
            name,
            executable_path,
        },
        native_handle,
        snapshot_pid: Some(pid),
    })
}

#[cfg(not(windows))]
const fn capture_restore_target_sync() -> Option<RestoreTarget> {
    None
}

#[cfg(windows)]
fn activate_hwnd_sync(
    handle: u64,
    snapshot_pid: Option<u32>,
    snapshot_exe: Option<&str>,
) -> std::result::Result<(), String> {
    use windows_sys::Win32::Foundation::{GetLastError, HWND};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowThreadProcessId, IsIconic, IsWindow, SW_RESTORE, SetForegroundWindow, ShowWindow,
    };

    // SAFETY: round-trip via usize keeps the conversion lossless on
    // both pointer widths; `IsWindow` validates the handle before we
    // touch the window so a stale snapshot (target app closed between
    // palette open and paste) cannot crash. Windows can deny the focus
    // change (foreground lock, UAC integrity gap) but never crashes the
    // caller; we surface that denial as an error so the Tauri side aborts
    // the paste instead of injecting keystrokes into whatever window kept
    // focus.
    //
    // The `handle as usize` cast is *intentionally* truncating on 32-bit
    // Windows targets: `HWND` is pointer-sized, so on `i686-pc-windows-*`
    // it is 32 bits wide and a 64-bit `u64` carrying a real HWND already
    // fits in the low 32 bits. Allow the lint here because the truncation
    // matches the OS handle width on the target we're compiling for.
    #[allow(clippy::cast_possible_truncation)]
    unsafe {
        let hwnd = handle as usize as HWND;
        if hwnd.is_null() || IsWindow(hwnd) == 0 {
            return Err("restore target window no longer exists".into());
        }
        // Windows recycles HWNDs: the saved handle may now belong to an
        // unrelated process by the time the user picks an entry. Re-resolve
        // the owning PID and re-derive the executable path; if either
        // disagrees with the snapshot, abort instead of pasting into the
        // wrong process. (Same-process HWND reuse — a new top-level window
        // taking the same numeric handle within the *same* PID — would
        // slip past these two checks; the operator-visible payoff would
        // be the rare case where the original window already closed *and*
        // the OS happened to hand the integer back to the same process
        // before the user dismissed the palette. We accept that residual
        // risk rather than capture window-title / creation-time too, both
        // of which are themselves volatile mid-session.)
        if let Some(expected_pid) = snapshot_pid {
            let mut current_pid: u32 = 0;
            if GetWindowThreadProcessId(hwnd, &raw mut current_pid) == 0 || current_pid == 0 {
                return Err("could not resolve restore target owner".into());
            }
            if current_pid != expected_pid {
                return Err(format!(
                    "restore target hwnd reassigned to a different process (expected pid {expected_pid}, found {current_pid})"
                ));
            }
            if let Some(expected_exe) = snapshot_exe {
                match query_process_image_path(current_pid) {
                    Some(current_exe) if current_exe == expected_exe => {}
                    Some(current_exe) => {
                        return Err(format!(
                            "restore target executable changed (expected {expected_exe}, found {current_exe})"
                        ));
                    }
                    None => {
                        return Err("could not re-resolve restore target executable path".into());
                    }
                }
            }
        }
        // Minimised windows refuse `SetForegroundWindow`; restore first
        // so the user's paste actually lands somewhere visible. We check
        // `IsIconic` (not `IsWindowVisible`) because the Win32 visibility
        // bit stays set while a window is minimised — only `IsIconic`
        // reliably distinguishes the minimised state. The `ShowWindow`
        // return value is the *previous* visibility; we don't care about
        // it, we just need to undo the minimise.
        if IsIconic(hwnd) != 0 {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        // A zero return means the foreground change was denied (the target
        // never came to the front). Reporting it lets the caller stop
        // before `SendInput` sprays the paste into whatever window still
        // owns focus. `GetLastError` is occasionally `0` here even on
        // denial, so we only attach it when it carries a code.
        if SetForegroundWindow(hwnd) == 0 {
            let code = GetLastError();
            return if code == 0 {
                Err("failed to bring restore target to the foreground".into())
            } else {
                Err(format!(
                    "failed to bring restore target to the foreground (GetLastError {code})"
                ))
            };
        }
    }
    Ok(())
}

#[cfg(not(windows))]
#[allow(clippy::unnecessary_wraps)] // signature must match the windows variant
const fn activate_hwnd_sync(
    _handle: u64,
    _snapshot_pid: Option<u32>,
    _snapshot_exe: Option<&str>,
) -> std::result::Result<(), String> {
    Ok(())
}

#[cfg(windows)]
unsafe fn query_process_image_path(pid: u32) -> Option<String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, GetLastError};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };

    // Win32 "long path" cap is 32,767 wchars; everything beyond that is
    // a sign the OS itself can no longer hand us a path. Start with the
    // historical 1024-wchar buffer (covers virtually every process) and
    // double on ERROR_INSUFFICIENT_BUFFER so long-path-enabled systems
    // with deeply nested install roots still get source attribution
    // instead of silently dropping back to `Unknown`.
    const INITIAL_BUF: usize = 1024;
    const MAX_BUF: usize = 32_768;

    // SAFETY: `OpenProcess` returns a HANDLE we explicitly `CloseHandle`
    // on every return path. The buffer outlives the call; `size` is an
    // in/out parameter that comes back capped at the buffer length on
    // success.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut cap = INITIAL_BUF;
        let result = loop {
            let mut buf = vec![0_u16; cap];
            // `cap` is bounded by `MAX_BUF` (32,768) so the conversion is
            // infallible; using `try_from` keeps clippy's truncation lint
            // satisfied without adding a runtime guard for a bound we own.
            let mut size: u32 = u32::try_from(cap).expect("cap fits in u32 (bounded by MAX_BUF)");
            let ok = QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                buf.as_mut_ptr(),
                &raw mut size,
            );
            if ok != 0 {
                buf.truncate(size as usize);
                break OsString::from_wide(&buf).into_string().ok();
            }
            if GetLastError() != ERROR_INSUFFICIENT_BUFFER || cap >= MAX_BUF {
                break None;
            }
            cap = (cap * 2).min(MAX_BUF);
        };
        CloseHandle(handle);
        result
    }
}

#[cfg(windows)]
unsafe fn query_window_title(hwnd: *mut core::ffi::c_void) -> Option<String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowTextLengthW, GetWindowTextW};

    // SAFETY: hwnd is the foreground window handle the caller just read;
    // the buffer is allocated to `len + 1` so `GetWindowTextW` can write
    // its terminating NUL without overflow.
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return None;
        }
        // `len` is positive here, so the i32 → usize conversion is safe;
        // `try_from` documents that and avoids clippy's truncation lint.
        let len_usize = usize::try_from(len).expect("len > 0 fits in usize");
        let cap = len_usize.saturating_add(1);
        let mut buf = vec![0_u16; cap];
        // Cap is `len + 1`; the +1 keeps it ≤ i32::MAX on every realistic
        // window title, but a defensive `try_from` still beats `as i32`.
        let cap_i32 = i32::try_from(cap).unwrap_or(i32::MAX);
        let written = GetWindowTextW(hwnd, buf.as_mut_ptr(), cap_i32);
        if written <= 0 {
            return None;
        }
        let written_usize = usize::try_from(written).expect("written > 0 fits in usize");
        buf.truncate(written_usize);
        OsString::from_wide(&buf).into_string().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_name_from_exe_takes_the_file_stem() {
        // Forward slashes split on every host, so these assertions hold on
        // the Windows runner and the macOS / Linux CI alike.
        assert_eq!(
            app_name_from_exe(Some("C:/Program Files/Notepad/notepad.exe")),
            Some("notepad".to_owned()),
        );
        assert_eq!(
            app_name_from_exe(Some("/usr/bin/foo")),
            Some("foo".to_owned())
        );
    }

    #[test]
    fn app_name_from_exe_handles_missing_or_stemless_paths() {
        assert_eq!(app_name_from_exe(None), None);
        // A bare separator has no file stem.
        assert_eq!(app_name_from_exe(Some("/")), None);
    }

    #[cfg(windows)]
    #[test]
    fn app_name_from_exe_splits_native_backslash_paths() {
        // The production callers pass Win32 paths, which use backslashes;
        // those only split as separators on Windows, so lock the real
        // behaviour down on the Windows runner.
        assert_eq!(
            app_name_from_exe(Some(r"C:\Program Files\Notepad\notepad.exe")),
            Some("notepad".to_owned()),
        );
    }
}
