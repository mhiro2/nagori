//! Settings, permissions, capabilities, capture-toggle, accessibility, and
//! the gated external-URL open.

use nagori_core::{AppError, Sensitivity};
use tauri::State;

use crate::dto::{
    AppDenyRuleDto, AppSettingsDto, HotkeyFailureDto, PermissionStatusDto, PlatformCapabilitiesDto,
};
use crate::error::{CommandError, CommandResult};
use crate::state::AppState;

use super::parse_entry_id;

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> CommandResult<AppSettingsDto> {
    // Read the body and its revision as one consistent pair. Two separate reads
    // could return body N with revision N+1 if a write landed in between, and
    // that stale body would then pass the compare-and-swap in `update_settings`
    // and revert the concurrent change.
    let (settings, revision) = state.runtime.get_settings_with_revision().await?;
    let mut dto: AppSettingsDto = settings.into();
    // Stamp the live optimistic-concurrency token so the window can echo it
    // back on the next `update_settings` as the compare-and-swap base.
    dto.revision = revision;
    Ok(dto)
}

/// Canonical password-manager preset bundled with the daemon. The
/// Settings UI calls this to learn what rules its "Block password
/// managers" toggle should add back when the user re-enables the
/// preset after disabling it — without a round trip the frontend
/// would have to keep its own copy of the preset list in sync with
/// `nagori-core`. Pure read; never touches state.
#[tauri::command]
pub fn password_manager_preset() -> Vec<AppDenyRuleDto> {
    nagori_core::password_manager_preset_rules()
        .into_iter()
        .map(Into::into)
        .collect()
}

#[tauri::command]
pub async fn update_settings(
    state: State<'_, AppState>,
    settings: AppSettingsDto,
) -> CommandResult<u64> {
    // The DTO's `revision` is the compare-and-swap base: the revision the
    // window last read via `get_settings` (or learned from a `settings_changed`
    // broadcast). The runtime rejects the write with `settings_conflict` if the
    // stored revision moved since then, so a stale full-blob snapshot cannot
    // revert a concurrent change (e.g. the tray's pause/resume) made in the
    // meantime. It is carried on the DTO rather than as a separate argument so
    // the wire shape stays a single settings object.
    let expected_revision = settings.revision;
    let value: nagori_core::AppSettings = settings.into();
    // Runtime persists the settings *and* re-publishes them on the watch
    // channel so the capture loop, maintenance task, and other subscribers
    // pick up the change without a second round-trip here. The returned
    // revision is the post-write token (callers may ignore it and rely on the
    // broadcast echo to refresh their baseline).
    let revision = state
        .runtime
        .save_settings_checked(value, expected_revision)
        .await?;
    Ok(revision)
}

/// Returns the current OS-level permission status. Used by the onboarding
/// view to surface "auto-paste OFF because Accessibility is missing" hints
/// without requiring the user to dive into the diagnostics CLI.
#[tauri::command]
pub async fn get_permissions(
    state: State<'_, AppState>,
) -> CommandResult<Vec<PermissionStatusDto>> {
    let statuses = state.runtime.permission_check().await?;
    Ok(statuses.into_iter().map(Into::into).collect())
}

/// Static, OS-level capability matrix wired in at runtime startup. Surfaced
/// to the frontend so the Settings → Status view can render "what could
/// work on this OS" alongside the dynamic `get_permissions` snapshot of
/// what currently *does* work. The runtime caches the matrix so this is a
/// cheap clone — safe to call on every Settings open.
#[tauri::command]
pub async fn get_capabilities(
    state: State<'_, AppState>,
) -> CommandResult<PlatformCapabilitiesDto> {
    Ok(state.runtime.capabilities().into())
}

/// Latest global-hotkey registration failure cached by the backend, or
/// `None` if the most recent (re-)registration succeeded. Used by the
/// always-on App-level subscriber to re-hydrate after a startup-race
/// emit: if the listener attached after the emit fired, the live event
/// is lost but the cached state survives. `nagori://hotkey_register_failed`
/// still fires for live updates.
#[allow(clippy::unused_async)]
#[tauri::command]
pub async fn last_hotkey_failure(
    state: State<'_, AppState>,
) -> CommandResult<Option<HotkeyFailureDto>> {
    Ok(state.current_hotkey_failure().map(Into::into))
}

/// Toggle `capture_enabled` without round-tripping the entire settings
/// blob — used by the tray menu's pause/resume entry.
#[tauri::command]
pub async fn set_capture_enabled(
    state: State<'_, AppState>,
    enabled: bool,
) -> CommandResult<AppSettingsDto> {
    let settings = state.runtime.set_capture_enabled(enabled).await?;
    Ok(settings.into())
}

/// Trigger the host's accessibility prompt and return the resulting
/// permission status. Wired to the Setup tab's `[ Grant Accessibility… ]`
/// button.
///
/// macOS: invokes `AXIsProcessTrustedWithOptions(prompt: true)` which
/// surfaces the TCC dialog asynchronously. The runtime stamps
/// `onboarding.accessibility_prompted_at` so the next
/// `permission_check` discriminates `Denied` from `NotDetermined`.
///
/// When `prompt = true` and TCC has *already* been asked previously
/// (i.e. `accessibility_prompted_at` is already `Some` before this
/// call), the OS suppresses the inline dialog. Falling back to `open(1)`
/// on the Privacy pane in that case gives the user a one-click route
/// to flip the toggle manually. On the very first prompt the inline
/// dialog is shown by the OS itself, so we deliberately skip the
/// fallback to avoid stacking a System Settings window on top of the
/// TCC dialog.
///
/// Windows / Linux: no analogous user-toggleable permission exists, so
/// the command returns the same curated status the doctor / Capability
/// table renders (granted-with-caveat for Windows UIPI, requires-wtype
/// for Linux). Returning a structured status rather than an
/// `Unsupported` error keeps the frontend code path symmetrical with
/// macOS.
#[cfg(target_os = "macos")]
#[tauri::command]
pub async fn request_accessibility(
    state: State<'_, AppState>,
    prompt: bool,
) -> CommandResult<PermissionStatusDto> {
    // Snapshot prompt history BEFORE the runtime call. The runtime
    // unconditionally stamps `accessibility_prompted_at` on
    // `prompt = true`, so reading after the call would always see the
    // fresh timestamp and never trigger the System Settings fallback.
    let previously_prompted = state
        .runtime
        .current_settings()
        .onboarding
        .accessibility_prompted_at
        .is_some();
    let status = state.runtime.request_accessibility(prompt).await?;
    if prompt && previously_prompted && status.state != nagori_platform::PermissionState::Granted {
        // TCC suppressed the dialog because it remembers a prior
        // Deny / dismiss; the Privacy pane is the user's only
        // remaining route. Surface a failed `open(1)` as a command
        // error so the Setup card can render it inline (§3.4) instead
        // of silently dropping the user's only escape hatch.
        //
        // `open(1)` is a synchronous fork+exec+wait, so run it via
        // `spawn_blocking` to keep the fork off the async worker thread.
        let open_status = tauri::async_runtime::spawn_blocking(|| {
            std::process::Command::new("open")
                .arg(
                    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
                )
                .status()
        })
        .await
        .map_err(|err| CommandError::internal(format!("the open task did not complete: {err}")))?
        .map_err(|err| {
            CommandError::internal(format!("failed to open the Accessibility pane: {err}"))
        })?;
        if !open_status.success() {
            return Err(CommandError::internal(format!(
                "the Accessibility pane failed to open ({open_status})"
            )));
        }
    }
    Ok(status.into())
}

#[cfg(target_os = "windows")]
#[tauri::command]
pub async fn request_accessibility(
    state: State<'_, AppState>,
    prompt: bool,
) -> CommandResult<PermissionStatusDto> {
    let status = state.runtime.request_accessibility(prompt).await?;
    Ok(status.into())
}

#[cfg(target_os = "linux")]
#[tauri::command]
pub async fn request_accessibility(
    state: State<'_, AppState>,
    prompt: bool,
) -> CommandResult<PermissionStatusDto> {
    let status = state.runtime.request_accessibility(prompt).await?;
    Ok(status.into())
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
#[tauri::command]
pub async fn request_accessibility(
    _state: State<'_, AppState>,
    _prompt: bool,
) -> CommandResult<PermissionStatusDto> {
    Err(CommandError::unsupported("request_accessibility"))
}

/// Allowlisted URL schemes for the `open_url_external` external-open
/// gate. The renderer hides the "Enter to open" hint for everything else,
/// but we duplicate the check here so a forged invoke can't escape via a
/// custom scheme handler the user wouldn't expect. `mailto:` was on the
/// original plan but the core URL classifier only tags `http(s)` clips
/// as `ClipboardContent::Url`, so a `mailto:` body never reaches this
/// code path today — re-add it here alongside the classifier change.
const URL_SCHEME_ALLOWLIST: &[&str] = &["https", "http"];

/// Open a URL belonging to a Public entry in the user's default browser.
/// The renderer pre-confirms the host so the user has explicit intent;
/// this handler re-verifies sensitivity and scheme server-side so a
/// forged invoke cannot ferry a Secret body out via the system handler.
/// The `url` argument must match the entry's stored URL — we re-fetch
/// the entry and compare to defend against a compromised renderer
/// claiming any arbitrary target.
#[tauri::command]
pub async fn open_url_external(
    state: State<'_, AppState>,
    entry_id: String,
    url: String,
) -> CommandResult<()> {
    let entry_id = parse_entry_id(&entry_id)?;
    let entry = state
        .runtime
        .get_entry(entry_id)
        .await?
        .ok_or(AppError::NotFound)?;
    // Sensitivity gate first — never reach the OS handler for a
    // Private/Secret/Blocked clip even if the renderer asks. The preview
    // pane already hides the Enter hint for these, but a forged invoke
    // could still arrive (e.g. via DevTools in a debug build).
    let canonical = validate_external_open(&entry, &url)?;
    open_external_url(canonical).await?;
    Ok(())
}

/// Pure gate for `open_url_external` — extracted so the
/// sensitivity / kind / URL-match / scheme-allowlist checks can be
/// exercised without spinning up a runtime. Returns the canonical
/// (parsed-and-re-serialised) URL that should be handed to the platform
/// opener so the rest of the command never re-uses the raw renderer
/// string.
fn validate_external_open(
    entry: &nagori_core::ClipboardEntry,
    requested_url: &str,
) -> Result<String, CommandError> {
    if !matches!(entry.sensitivity, Sensitivity::Public) {
        return Err(CommandError::forbidden(
            "external open is only available for Public entries",
        ));
    }
    let nagori_core::ClipboardContent::Url(stored) = &entry.content else {
        return Err(CommandError::invalid_input("entry is not a URL clip"));
    };
    // Compare against the entry's stored URL so a compromised renderer
    // can't redirect to an attacker-controlled URL while presenting the
    // user's confirm dialog with the legitimate host.
    if requested_url.trim() != stored.raw.trim() {
        return Err(CommandError::invalid_input(
            "url does not match the stored entry",
        ));
    }
    let parsed = url::Url::parse(requested_url.trim())
        .map_err(|err| CommandError::invalid_input(format!("invalid url: {err}")))?;
    let scheme = parsed.scheme();
    if !URL_SCHEME_ALLOWLIST.contains(&scheme) {
        return Err(CommandError::invalid_input(format!(
            "scheme `{scheme}` is not allowed for external open"
        )));
    }
    Ok(parsed.as_str().to_owned())
}

/// Hand the URL to the platform's default URL handler. We shell out
/// directly (mirroring `request_accessibility`'s open-settings fallback) rather than wiring
/// `tauri-plugin-shell`'s JS surface — every call site here is already
/// inside a Rust command that has run the full sensitivity / allowlist
/// gate, so the plugin's capability layer would be redundant overhead.
///
/// Windows uses `ShellExecuteW` directly instead of `cmd /c start`: the
/// canonical URL has already been validated against the entry's stored
/// claim, but `cmd.exe` interprets `&`, `^`, `|`, etc. on its argument
/// strings before invoking `start`, so a future allowlist relaxation
/// (or a URL whose query contains those characters) could turn a benign
/// argument into a shell metacharacter. `ShellExecuteW` skips the shell
/// parser entirely.
///
/// Runs on a blocking thread: `open(1)` / `xdg-open` are synchronous
/// fork+exec+wait spawns and `ShellExecuteW` can block while it resolves a
/// handler, so doing this inline would stall the async worker thread.
async fn open_external_url(url: String) -> CommandResult<()> {
    tauri::async_runtime::spawn_blocking(move || open_external_url_blocking(&url))
        .await
        .map_err(|err| CommandError::internal(format!("the open task did not complete: {err}")))?
}

fn open_external_url_blocking(url: &str) -> CommandResult<()> {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let mut command = Command::new("open");
        // `--` stops `open(1)` from interpreting a URL beginning with a
        // dash as one of its own flags, even though the upstream parser
        // is currently strict about that — keeps us safe across releases.
        command.arg("--").arg(url);
        let status = command
            .status()
            .map_err(|err| CommandError::internal(err.to_string()))?;
        if !status.success() {
            // Mirror the Windows branch and the paste-failure UX: surface the
            // failure to the WebView so the user sees a toast instead of a
            // silent no-op when LaunchServices refuses the URL.
            return Err(CommandError::internal(format!(
                "open(1) exited with {status}"
            )));
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        let mut command = Command::new("xdg-open");
        command.arg(url);
        let status = command
            .status()
            .map_err(|err| CommandError::internal(err.to_string()))?;
        if !status.success() {
            // `xdg-open` exits non-zero for missing handlers, syntax errors,
            // and tool-not-found shims — none of which should look like
            // success to the WebView.
            return Err(CommandError::internal(format!(
                "xdg-open exited with {status}"
            )));
        }
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use std::ptr::null;
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let operation: Vec<u16> = std::ffi::OsStr::new("open")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let file: Vec<u16> = std::ffi::OsStr::new(url)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `operation` and `file` are NUL-terminated UTF-16 strings
        // that outlive the call. `ShellExecuteW` returns an HINSTANCE that
        // is `> 32` on success per the documented contract. The desktop
        // crate inherits the workspace `unsafe_code = "deny"` lint, so the
        // single Win32 FFI here is opted in locally rather than relaxing
        // the whole crate; the call site is otherwise pure Rust glue.
        #[allow(unsafe_code)]
        let hinstance = unsafe {
            ShellExecuteW(
                null::<core::ffi::c_void>() as _,
                operation.as_ptr(),
                file.as_ptr(),
                null(),
                null(),
                SW_SHOWNORMAL,
            )
        };
        if (hinstance as isize) <= 32 {
            return Err(CommandError::internal(format!(
                "ShellExecuteW failed with code {}",
                hinstance as isize
            )));
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = url;
        Err(CommandError::unsupported(
            "external URL open is not supported on this platform",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url_entry(raw: &str) -> nagori_core::ClipboardEntry {
        use nagori_core::{
            ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
            ContentHash, EntryFactory,
        };
        use time::OffsetDateTime;
        let snapshot = ClipboardSnapshot {
            sequence: ClipboardSequence::content_hash(ContentHash::sha256(raw.as_bytes()).value),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text(raw.to_owned()),
            }],
        };
        let mut entry = EntryFactory::from_snapshot(snapshot).expect("url snapshot");
        // `from_snapshot` defaults `sensitivity` to `Unknown`; the gate
        // requires an explicit `Public` clip, so set it here for the
        // accept/match/scheme paths and let the negative test override it.
        entry.sensitivity = Sensitivity::Public;
        entry
    }

    #[test]
    fn validate_external_open_accepts_public_https_match() {
        let entry = url_entry("https://example.com/foo?bar=1");
        let canonical = validate_external_open(&entry, "https://example.com/foo?bar=1")
            .expect("public https url is accepted");
        assert!(canonical.starts_with("https://example.com/"));
    }

    #[test]
    fn validate_external_open_rejects_non_public_entries_with_forbidden() {
        // Sensitivity gate must trip before the URL-match check so a
        // forged invoke against a Secret entry can never reach the OS
        // handler — even if the renderer happened to know the URL.
        let mut entry = url_entry("https://example.com/foo");
        entry.sensitivity = Sensitivity::Secret;
        let err = validate_external_open(&entry, "https://example.com/foo")
            .expect_err("secret entries are blocked");
        assert_eq!(err.code, "forbidden");
        assert!(!err.recoverable);
    }

    #[test]
    fn validate_external_open_rejects_url_mismatch() {
        // The renderer-supplied URL must equal the stored one byte-for-byte
        // (after trim) so a compromised webview can't redirect to an
        // attacker-controlled host while displaying the legitimate confirm.
        let entry = url_entry("https://example.com/foo");
        let err = validate_external_open(&entry, "https://attacker.test/foo")
            .expect_err("mismatched url is rejected");
        assert_eq!(err.code, "invalid_input");
        assert!(err.message.contains("does not match"));
    }

    #[test]
    fn validate_external_open_rejects_disallowed_schemes() {
        // `file://`, `javascript:`, etc. must never reach the platform
        // handler — even on a Public clip with a matching URL string —
        // because the system handler interprets them in ways the user
        // does not expect from a clipboard preview.
        // Constructing a `file://` ClipboardEntry isn't possible (the
        // core URL parser only accepts http/https), so we exercise the
        // scheme gate through a hand-rolled mismatch fixture:
        let mut entry = url_entry("https://example.com/foo");
        // Swap the stored URL to a file:// scheme so the scheme gate
        // becomes the failure surface, not the URL-match check.
        if let nagori_core::ClipboardContent::Url(stored) = &mut entry.content {
            stored.raw = "file:///etc/passwd".to_owned();
        }
        let err = validate_external_open(&entry, "file:///etc/passwd")
            .expect_err("file scheme is rejected");
        assert_eq!(err.code, "invalid_input");
        assert!(
            err.message.contains("not allowed"),
            "expected scheme-allowlist message, got {:?}",
            err.message,
        );
    }
}
