use async_trait::async_trait;
use core_foundation::base::{CFType, TCFType};
use core_foundation::string::CFString;
use nagori_core::{Result, SourceApp};
use nagori_platform::{FrontmostApp, RestoreTarget, WindowBehavior};
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};
use objc2_foundation::NSString;

/// Thread-safety notes for the `AppKit` calls used here.
///
/// `NSWorkspace::sharedWorkspace`, `frontmostApplication`, and
/// `runningApplicationsWithBundleIdentifier` are documented as thread-safe:
/// they read snapshots of the workspace state under `AppKit`'s internal
/// locking and do not require a `CFRunLoop`. `activateWithOptions` is also
/// safe to call from a background thread — internally it posts to the main
/// thread via the `AppleEvent` dispatch.
///
/// We still hop to a blocking thread (rather than running on a tokio worker
/// directly) because each call can take a few ms when `AppKit`'s lock is
/// contended, and pinning a tokio worker for that long starves the IPC
/// handler. The blocking pool is the right place for short, sync FFI work.
#[derive(Debug, Default)]
pub struct MacosWindowBehavior;

impl MacosWindowBehavior {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Synchronous variant of `frontmost_app` so callers running in a
    /// non-async context (e.g. the Tauri global-shortcut handler) can
    /// snapshot the previous frontmost without spinning up a runtime.
    /// Callers must already be on a thread where blocking on `AppKit` is OK
    /// (typically the global-shortcut callback, which runs off the main
    /// thread but outside tokio).
    #[must_use]
    pub fn frontmost_app_blocking() -> Option<FrontmostApp> {
        frontmost_app_sync()
    }

    /// Capture a [`RestoreTarget`] at palette-open time. On macOS the
    /// restore handle is always the bundle id carried by `SourceApp`, so
    /// `native_handle` stays `None` and the cross-platform default impl
    /// of `activate_restore_target` (which dispatches on `bundle_id`)
    /// does the right thing. We still ship the helper so callers in the
    /// desktop shell can stay platform-agnostic.
    #[must_use]
    pub fn capture_restore_target_blocking() -> Option<RestoreTarget> {
        frontmost_app_sync().map(|front| RestoreTarget {
            source: front.source,
            native_handle: None,
        })
    }
}

#[async_trait]
impl WindowBehavior for MacosWindowBehavior {
    async fn frontmost_app(&self) -> Result<Option<FrontmostApp>> {
        // Hop off the tokio worker so a contended AppKit lock can't stall
        // IPC handlers running in parallel.
        tokio::task::spawn_blocking(frontmost_app_sync)
            .await
            .map_err(|err| nagori_core::AppError::Platform(err.to_string()))
    }

    // The Tauri shell controls the palette window directly via its own
    // commands; the daemon-side `WindowBehavior` only reports frontmost-app
    // metadata, so show / hide are no-ops here.
    async fn show_palette(&self) -> Result<()> {
        Ok(())
    }

    async fn hide_palette(&self) -> Result<()> {
        Ok(())
    }

    async fn activate_app(&self, bundle_id: &str) -> Result<()> {
        let bundle_id = bundle_id.to_owned();
        tokio::task::spawn_blocking(move || activate_app_sync(&bundle_id))
            .await
            .map_err(|err| nagori_core::AppError::Platform(err.to_string()))?;
        Ok(())
    }

    async fn frontmost_focused_is_secure(&self) -> Result<bool> {
        // The AX round-trip can stall on a misbehaving frontmost process,
        // and the call is synchronous, so route through the blocking pool
        // for the same reason `frontmost_app` does. The blocking helper
        // returns `None` when the AX query itself failed (permission
        // revoked, opaque element, transient FFI error). We surface that
        // as `Err` so the capture loop's `consecutive_secure_ax_failures`
        // counter increments and the fail-closed threshold can fire —
        // before this fix, AX errors silently coerced to `Ok(false)` and
        // the counter never advanced.
        let outcome = tokio::task::spawn_blocking(frontmost_focused_is_secure_sync)
            .await
            .map_err(|err| nagori_core::AppError::Platform(err.to_string()))?;
        outcome.ok_or_else(|| {
            nagori_core::AppError::Platform(
                "AX query for focused-element role failed; treat as unknown for fail-closed"
                    .to_owned(),
            )
        })
    }
}

fn frontmost_app_sync() -> Option<FrontmostApp> {
    let workspace = NSWorkspace::sharedWorkspace();
    let app: objc2::rc::Retained<NSRunningApplication> = workspace.frontmostApplication()?;
    let bundle_id = app.bundleIdentifier().map(|s| s.to_string());
    let name = app.localizedName().map(|s| s.to_string());
    Some(FrontmostApp {
        source: SourceApp {
            bundle_id,
            name,
            executable_path: None,
        },
        window_title: None,
    })
}

fn activate_app_sync(bundle_id: &str) {
    let ns_id = NSString::from_str(bundle_id);
    let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&ns_id);
    let Some(app) = apps.iter().next() else {
        // Caller treats "app no longer running" as best-effort, so this
        // is not promoted to an error — the paste will land wherever the
        // OS now considers frontmost.
        return;
    };
    #[allow(deprecated)]
    let _ = app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
}

/// Walk the system-wide Accessibility tree to learn whether the frontmost
/// app's focused element is a secure text field.
///
/// Tri-state semantics:
///   * `Some(true)`  — AX confirmed the focused element is `AXSecureTextField`.
///   * `Some(false)` — AX answered cleanly and the element is not secure.
///   * `None`        — the AX query itself failed (missing Accessibility
///     permission, system-wide handle null, focused-element fetch
///     errored, role/subrole copy returned null). Callers must treat this
///     as "unknown", not "definitely not secure", so the capture loop can
///     fail closed after a sustained run of failures.
///
/// Earlier versions returned plain `bool` and collapsed every AX failure
/// into `false`, which meant the `consecutive_secure_ax_failures` counter
/// in the capture loop never advanced past zero — and therefore never
/// crossed the fail-closed threshold even with Accessibility permission
/// fully revoked.
fn frontmost_focused_is_secure_sync() -> Option<bool> {
    // Both the role and subrole forms of the constant resolve to the
    // same string ("AXSecureTextField"); checking each separately
    // covers apps that surface only one and keeps us forward-
    // compatible with hosts that promote secure-field semantics to
    // either slot.
    const SECURE: &str = "AXSecureTextField";
    // SAFETY: Each AX call below is documented thread-safe; the FFI
    // signatures match Apple's headers, and every Create-rule pointer
    // either flows into `wrap_under_create_rule` (which will release
    // through `Drop`) or is released explicitly with `CFRelease` before
    // we leave the unsafe block. We never deref a raw pointer past the
    // point we release it.
    unsafe {
        let systemwide = ax_ffi::AXUIElementCreateSystemWide();
        if systemwide.is_null() {
            return None;
        }
        // Bound the per-element AX trip so an unresponsive focused app
        // can't stall the capture loop's polling tick. Apple's docs note
        // 6 s is the default; 0.25 s is more than 100x what a healthy
        // app needs and small enough to absorb at our 500 ms cadence.
        // Errors here are non-fatal — we still proceed with the (longer)
        // default timeout.
        let _ = ax_ffi::AXUIElementSetMessagingTimeout(systemwide, 0.25);

        // `AXFocusedUIElement` is the AX-permission gate: if Accessibility
        // is not granted, this fetch fails, and we want that to surface as
        // an `Err` from the trait method so the capture-loop counter
        // ticks. Returning `Some(false)` here would silently fail-open.
        let Some(focused) = copy_element_attribute(systemwide, "AXFocusedUIElement") else {
            CFRelease(systemwide.cast());
            return None;
        };
        CFRelease(systemwide.cast());

        let role = copy_string_attribute(focused, "AXRole");
        let subrole = copy_string_attribute(focused, "AXSubrole");
        CFRelease(focused.cast());

        // If both attributes came back null the AX result is genuinely
        // unknown: a non-AX-aware focused element, or a transient framework
        // error. Treat that as "unknown" so the counter advances on
        // sustained outages. Apps that intentionally don't expose either
        // slot will still ship `Some(false)` after the role/subrole copy
        // returns a non-secure value, which is the steady-state path.
        match (role.as_deref(), subrole.as_deref()) {
            (Some(role), Some(subrole)) => Some(role == SECURE || subrole == SECURE),
            (Some(role), None) => Some(role == SECURE),
            (None, Some(subrole)) => Some(subrole == SECURE),
            (None, None) => None,
        }
    }
}

/// Copy a child `AXUIElementRef` attribute, transferring the +1 retain
/// to the caller. The returned pointer must be released with
/// `CFRelease` — we keep it raw rather than wrapping in `CFType`
/// because `AXUIElementRef` is an opaque type that the `core-foundation`
/// crate does not model.
unsafe fn copy_element_attribute(
    element: ax_ffi::AXUIElementRef,
    name: &str,
) -> Option<ax_ffi::AXUIElementRef> {
    let attr = CFString::new(name);
    let mut raw: ax_ffi::CFTypeRef = std::ptr::null();
    // SAFETY: `attr` is alive for the call; `&mut raw` is a valid out-
    // pointer for the +1-retained CF result.
    let err = unsafe {
        ax_ffi::AXUIElementCopyAttributeValue(
            element,
            attr.as_concrete_TypeRef().cast(),
            &raw mut raw,
        )
    };
    if err != ax_ffi::AX_ERROR_SUCCESS || raw.is_null() {
        return None;
    }
    // The opaque AX type the AX framework returns is conceptually a
    // mutable handle; cast away the const that CFTypeRef carries.
    Some(raw.cast_mut())
}

/// Copy a string-valued AX attribute. The result is auto-released via
/// `core_foundation::base::CFType`'s `Drop`, so callers don't need to
/// remember to free it.
unsafe fn copy_string_attribute(element: ax_ffi::AXUIElementRef, name: &str) -> Option<String> {
    let attr = CFString::new(name);
    let mut raw: ax_ffi::CFTypeRef = std::ptr::null();
    let err = unsafe {
        ax_ffi::AXUIElementCopyAttributeValue(
            element,
            attr.as_concrete_TypeRef().cast(),
            &raw mut raw,
        )
    };
    if err != ax_ffi::AX_ERROR_SUCCESS || raw.is_null() {
        return None;
    }
    // Take ownership of the +1 retain so the value drops at the end of
    // this scope no matter which return path runs.
    let value = unsafe { CFType::wrap_under_create_rule(raw) };
    value.downcast::<CFString>().map(|s| s.to_string())
}

#[cfg(target_os = "macos")]
mod ax_ffi {
    use core::ffi::{c_int, c_void};

    pub type AXUIElementRef = *mut c_void;
    pub type CFTypeRef = *const c_void;
    pub type CFStringRef = *const c_void;
    pub type AXError = c_int;

    pub const AX_ERROR_SUCCESS: AXError = 0;

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        pub fn AXUIElementCreateSystemWide() -> AXUIElementRef;
        pub fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: *mut CFTypeRef,
        ) -> AXError;
        pub fn AXUIElementSetMessagingTimeout(
            element: AXUIElementRef,
            timeout_in_seconds: f32,
        ) -> AXError;
    }
}

// `CFRelease` is declared inline rather than pulled from `core-foundation-sys`
// to keep the explicit dependency surface to the higher-level
// `core-foundation` crate already used above for safe CFType handling.
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const core::ffi::c_void);
}
