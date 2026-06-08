use async_trait::async_trait;
use core_foundation::base::{CFRelease, CFType, TCFType};
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
///
/// Each hop is also bounded by [`WINDOW_OP_TIMEOUT`] so a *wedged* (not merely
/// contended) `AppKit` lock cannot leave the `spawn_blocking` pending forever —
/// the detached worker is leaked until the lock frees, but the async caller is
/// released within the window.
#[derive(Debug, Default)]
pub struct MacosWindowBehavior;

/// Upper bound on a blocking `NSWorkspace` / `activateWithOptions` call.
/// Frontmost-app probing happens at palette-open and focus restore happens
/// just before the synthesised ⌘V. A healthy frontmost probe answers in a few
/// ms; focus restore additionally polls up to [`ACTIVATION_VERIFY_DEADLINE`]
/// (500 ms) for the target to become frontmost, so 3 s leaves headroom for that
/// verification while still bounding a wedged `AppKit` lock.
const WINDOW_OP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

impl MacosWindowBehavior {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Synchronous variant of `frontmost_app` so callers running in a
    /// non-async context (e.g. the Tauri global-shortcut handler) can
    /// snapshot the previous frontmost without spinning up a runtime.
    ///
    /// **Never call this from inside a tokio task / async fn.** The
    /// implementation acquires `AppKit`'s internal lock, which can be
    /// held for several ms under contention; running it on a tokio
    /// worker thread parks that worker for the duration and starves
    /// every other future scheduled on the same runtime (IPC handlers,
    /// capture loop ticks). The async [`WindowBehavior::frontmost_app`]
    /// impl deliberately hops through `spawn_blocking` for exactly this
    /// reason — call that path instead. This blocking entry point is
    /// intended for callers that are *already* off the tokio runtime
    /// (e.g. the global-shortcut callback, which is dispatched on a
    /// dedicated OS thread).
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
            snapshot_pid: None,
        })
    }
}

#[async_trait]
impl WindowBehavior for MacosWindowBehavior {
    async fn frontmost_app(&self) -> Result<Option<FrontmostApp>> {
        // Hop off the tokio worker so a contended AppKit lock can't stall
        // IPC handlers running in parallel, and bound it so a *wedged* lock
        // (a frozen frontmost app) can't leave the hop pending forever.
        nagori_platform::run_blocking_with_timeout(
            "frontmost_app",
            WINDOW_OP_TIMEOUT,
            frontmost_app_sync,
        )
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
        // Focus restore (`activateWithOptions`) runs after the palette hides
        // and before the synthesised ⌘V. Bound it so a wedged AppKit lock
        // can't hang the paste flow — on timeout we surface a platform error
        // and the desktop aborts the paste rather than letting ⌘V land in
        // whatever window kept focus.
        //
        // `activate_app_sync` also verifies the activation actually took: it
        // checks `activateWithOptions`' return value and then polls until the
        // workspace reports the target as frontmost. A terminated / unfocusable
        // target therefore surfaces as an `Err` instead of a silent success, so
        // the desktop's synthesise path aborts the ⌘V rather than typing the
        // clipboard into whatever window happened to keep focus.
        let outcome = nagori_platform::run_blocking_with_timeout(
            "activate_app",
            WINDOW_OP_TIMEOUT,
            move || activate_app_sync(&bundle_id),
        )
        .await
        .map_err(|err| nagori_core::AppError::Platform(err.to_string()))?;
        outcome.map_err(nagori_core::AppError::Platform)
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

/// How long to wait for `activateWithOptions` to actually move the frontmost
/// app to the target. The call returns immediately and posts the activation to
/// the main thread, so the frontmost app may not have changed yet on return. A
/// healthy hand-off lands in tens of ms; the desktop then sleeps a further
/// 60 ms before synthesising ⌘V. If the target never becomes frontmost within
/// this window we abort so the keystroke is not sent into the wrong window.
const ACTIVATION_VERIFY_DEADLINE: std::time::Duration = std::time::Duration::from_millis(500);
/// Poll cadence while waiting for the frontmost app to flip to the target.
const ACTIVATION_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);

/// Activate the app identified by `bundle_id` and verify the activation took.
///
/// Returns `Err(reason)` when the target is no longer running, when
/// `activateWithOptions` reports failure, or when the target does not become
/// the frontmost app within [`ACTIVATION_VERIFY_DEADLINE`]. The caller maps the
/// reason onto `AppError::Platform` so the paste flow can abort the synthesised
/// ⌘V rather than send it into whatever window kept focus.
fn activate_app_sync(bundle_id: &str) -> std::result::Result<(), String> {
    let ns_id = NSString::from_str(bundle_id);
    let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&ns_id);
    let Some(app) = apps.iter().next() else {
        return Err(format!(
            "focus restore target {bundle_id} is no longer running"
        ));
    };
    #[allow(deprecated)]
    let activated = app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
    if !activated {
        return Err(format!(
            "activateWithOptions reported failure for {bundle_id} (target terminated or cannot be activated)"
        ));
    }
    // The activation is posted to the main thread, so poll the workspace until
    // it reports the target as frontmost before declaring success. `Instant`
    // is safe here: this is a sub-second active spin, not a wall-clock gap that
    // could span a system sleep.
    let deadline = std::time::Instant::now() + ACTIVATION_VERIFY_DEADLINE;
    loop {
        if frontmost_bundle_id().as_deref() == Some(bundle_id) {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "{bundle_id} did not become frontmost within {} ms after activation",
                ACTIVATION_VERIFY_DEADLINE.as_millis()
            ));
        }
        std::thread::sleep(ACTIVATION_POLL_INTERVAL);
    }
}

/// Bundle id of the workspace's current frontmost app, if any. A lean read used
/// to verify a focus restore actually moved the frontmost to the target.
fn frontmost_bundle_id() -> Option<String> {
    let workspace = NSWorkspace::sharedWorkspace();
    let app = workspace.frontmostApplication()?;
    app.bundleIdentifier().map(|s| s.to_string())
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

        let role_outcome = copy_string_attribute(focused, "AXRole");
        let subrole_outcome = copy_string_attribute(focused, "AXSubrole");
        CFRelease(focused.cast());

        // `AXFocusedUIElement` already cleared the AX-permission gate,
        // so reaching here means the system handed us a valid focused
        // element. The role/subrole calls can still fail in two
        // qualitatively different ways:
        //
        //   * `AttrOutcome::Unsupported` — AX answered "this element
        //     doesn't expose this attribute" (kAXErrorAttributeUnsupported
        //     or kAXErrorNoValue). This is the thin-AX surface: an
        //     Electron window without proper AX wiring, a GPU-rendered
        //     game, a custom Cocoa control that never set
        //     `accessibilityRole`. A secure text field, by contrast, must
        //     vend AXRole = "AXSecureTextField"; the only safe reading is
        //     "not a secure field" — `Some(false)`. Anything else would
        //     tick `consecutive_secure_ax_failures` on every poll spent
        //     in a perfectly safe non-AX app.
        //
        //   * `AttrOutcome::Failed` — AX returned a transient error
        //     (kAXErrorCannotComplete timeout, kAXErrorInvalidUIElement
        //     stale handle, kAXErrorAPIDisabled mid-call, downcast
        //     mismatch). The element exists and *could* be secure;
        //     surfacing `None` lets the capture loop's fail-closed
        //     threshold absorb the failure.
        match (role_outcome, subrole_outcome) {
            (AttrOutcome::Failed, _) | (_, AttrOutcome::Failed) => None,
            (role, subrole) => {
                let role_secure = matches!(role, AttrOutcome::Value(ref s) if s == SECURE);
                let subrole_secure = matches!(subrole, AttrOutcome::Value(ref s) if s == SECURE);
                Some(role_secure || subrole_secure)
            }
        }
    }
}

/// String-attribute fetch outcome that preserves the distinction between
/// "AX said the attribute is unavailable" and "AX hit a transient error".
/// Collapsing both into `None` would force the caller to choose between
/// fail-open (drift the secret detector past every AX-poor regular app)
/// and fail-closed (escalate genuine permission flickers into snapshots
/// dropped at the threshold). The split lets the caller pick per case.
enum AttrOutcome {
    Value(String),
    /// AX reported the attribute is structurally unavailable on this
    /// element (`kAXErrorAttributeUnsupported` or `kAXErrorNoValue`).
    /// Read as "definitely not this attribute" for the SECURE check.
    Unsupported,
    /// AX returned a transient or coding error (timeout, stale element,
    /// API disabled, type mismatch). Treat as "unknown" upstream.
    Failed,
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

/// Copy a string-valued AX attribute, classifying the result so the
/// caller can tell a thin-AX surface from a transient AX failure.
///
/// `AttrOutcome::Unsupported` fires on `kAXErrorAttributeUnsupported`
/// and `kAXErrorNoValue` (and on success-with-null-pointer, which is
/// the same shape). `AttrOutcome::Failed` covers every other non-zero
/// AX error code plus the case where AX returned a non-string value.
/// The result is auto-released via `core_foundation::base::CFType`'s
/// `Drop`, so callers don't need to remember to free it.
unsafe fn copy_string_attribute(element: ax_ffi::AXUIElementRef, name: &str) -> AttrOutcome {
    let attr = CFString::new(name);
    let mut raw: ax_ffi::CFTypeRef = std::ptr::null();
    let err = unsafe {
        ax_ffi::AXUIElementCopyAttributeValue(
            element,
            attr.as_concrete_TypeRef().cast(),
            &raw mut raw,
        )
    };
    match err {
        ax_ffi::AX_ERROR_SUCCESS => {
            if raw.is_null() {
                return AttrOutcome::Unsupported;
            }
            // Take ownership of the +1 retain so the value drops at the
            // end of this scope no matter which arm runs.
            let value = unsafe { CFType::wrap_under_create_rule(raw) };
            value
                .downcast::<CFString>()
                .map_or(AttrOutcome::Failed, |s| AttrOutcome::Value(s.to_string()))
        }
        ax_ffi::AX_ERROR_ATTRIBUTE_UNSUPPORTED | ax_ffi::AX_ERROR_NO_VALUE => {
            AttrOutcome::Unsupported
        }
        _ => AttrOutcome::Failed,
    }
}

#[cfg(target_os = "macos")]
mod ax_ffi {
    use core::ffi::{c_int, c_void};

    pub type AXUIElementRef = *mut c_void;
    pub type CFTypeRef = *const c_void;
    pub type CFStringRef = *const c_void;
    pub type AXError = c_int;

    pub const AX_ERROR_SUCCESS: AXError = 0;
    /// `kAXErrorAttributeUnsupported`: element is well-formed but does
    /// not advertise this attribute.
    pub const AX_ERROR_ATTRIBUTE_UNSUPPORTED: AXError = -25205;
    /// `kAXErrorNoValue`: attribute is recognised but currently has no
    /// value (e.g. `AXFocusedUIElement` on a window with nothing focused).
    pub const AX_ERROR_NO_VALUE: AXError = -25212;

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
