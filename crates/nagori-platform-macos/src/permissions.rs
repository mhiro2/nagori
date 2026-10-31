use async_trait::async_trait;
use nagori_core::Result;
use nagori_platform::{
    BlockingError, PermissionCheckContext, PermissionChecker, PermissionKind, PermissionState,
    PermissionStatus, run_blocking_with_timeout,
};
use std::time::Duration;

const ACCESSIBILITY_SETUP_ROUTE: &str = "setup/accessibility";

/// Upper bound on how long a synchronous permission probe may block the
/// async runtime. `AXIsProcessTrusted` and `arboard::Clipboard::new` both
/// reach into the OS (the TCC database, the pasteboard server); a wedged
/// `WindowServer` or a stuck pasteboard would otherwise pin the tokio worker
/// for the whole `nagori doctor` / onboarding / Settings call. The clipboard
/// adapter already bounds its own ops via `CLIPBOARD_OP_TIMEOUT`; this mirrors
/// that for the probe path so the two never diverge.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Default)]
pub struct MacosPermissionChecker;

#[async_trait]
impl PermissionChecker for MacosPermissionChecker {
    async fn check(&self, ctx: &PermissionCheckContext) -> Result<Vec<PermissionStatus>> {
        let accessibility = accessibility_status(ctx).await;
        let clipboard = clipboard_status().await;
        Ok(vec![
            clipboard,
            accessibility,
            // InputMonitoring / Notifications / AutoLaunch don't have
            // user-mode probes that work without an entitlements bundle,
            // and the previous `NotDetermined` was indistinguishable from
            // "the OS hasn't asked yet" — which is misleading. Report
            // `Unsupported` so the doctor / onboarding views can render
            // "not probed" instead of "not yet asked".
            PermissionStatus {
                kind: PermissionKind::InputMonitoring,
                state: PermissionState::Unsupported,
                message: Some(
                    "InputMonitoring status cannot be probed without TCC entitlements".to_owned(),
                ),
                reason_code: None,
                setup_route: None,
                docs_url: None,
            },
            PermissionStatus {
                kind: PermissionKind::Notifications,
                state: PermissionState::Unsupported,
                message: Some("Notification authorization is not probed".to_owned()),
                reason_code: None,
                setup_route: None,
                docs_url: None,
            },
            PermissionStatus {
                kind: PermissionKind::AutoLaunch,
                state: PermissionState::Unsupported,
                message: Some("AutoLaunch state is managed by tauri-plugin-autostart".to_owned()),
                reason_code: None,
                setup_route: None,
                docs_url: None,
            },
        ])
    }

    async fn request_accessibility(&self, prompt: bool) -> Result<PermissionStatus> {
        // `AXIsProcessTrustedWithOptions` is a synchronous FFI call that may
        // surface the TCC dialog on the next event-loop pass; bound it on the
        // blocking pool so a wedged WindowServer can't pin the tokio worker.
        // `accessibility_status` already runs its probe this way — this keeps
        // the request path from diverging into a bare synchronous FFI call.
        let probe =
            run_blocking_with_timeout("macos_accessibility_request", PROBE_TIMEOUT, move || {
                accessibility_trusted_with_prompt(prompt)
            })
            .await;
        Ok(accessibility_request_row(probe))
    }
}

/// Map the outcome of an `AXIsProcessTrustedWithOptions` request onto the
/// Accessibility row returned to the caller.
fn accessibility_request_row(probe: std::result::Result<bool, BlockingError>) -> PermissionStatus {
    match probe {
        // A timed-out request can't be read as granted or denied; surface the
        // degraded row whose Setup card still deep-links the user into System
        // Settings.
        Err(err) => PermissionStatus {
            kind: PermissionKind::Accessibility,
            state: PermissionState::Denied,
            message: Some(format!(
                "Accessibility request did not complete ({})",
                err.describe()
            )),
            reason_code: Some("probe_timed_out".to_owned()),
            setup_route: Some(ACCESSIBILITY_SETUP_ROUTE.to_owned()),
            docs_url: None,
        },
        Ok(true) => granted_accessibility_row(),
        // `prompt = true` and `granted = false` means either:
        //   (a) the TCC dialog is now showing and the user has not
        //       responded yet, or
        //   (b) TCC has already recorded a Deny / unknown identity for
        //       this bundle so the dialog was suppressed.
        // Either way the actionable next step is the Setup card
        // (which deep-links into System Settings); the message stays
        // a terse CLI-friendly summary.
        Ok(false) => denied_accessibility_row(),
    }
}

const fn granted_accessibility_row() -> PermissionStatus {
    PermissionStatus {
        kind: PermissionKind::Accessibility,
        state: PermissionState::Granted,
        message: None,
        reason_code: None,
        setup_route: None,
        docs_url: None,
    }
}

fn denied_accessibility_row() -> PermissionStatus {
    PermissionStatus {
        kind: PermissionKind::Accessibility,
        state: PermissionState::Denied,
        message: Some("Accessibility not trusted".to_owned()),
        reason_code: Some("accessibility_not_trusted".to_owned()),
        setup_route: Some(ACCESSIBILITY_SETUP_ROUTE.to_owned()),
        docs_url: None,
    }
}

/// Build the Accessibility row of the permission report.
///
/// Without prompt history, `AXIsProcessTrusted() == false` collapses to
/// `NotDetermined` (we have never asked the OS to surface the TCC
/// dialog, so the user cannot meaningfully be said to have "denied"
/// anything). After the first
/// `AXIsProcessTrustedWithOptions(prompt: true)` call we begin treating
/// `false` as `Denied` so the Setup card switches its copy from
/// `NotRequested` to `PromptShownNotGranted`.
async fn accessibility_status(ctx: &PermissionCheckContext) -> PermissionStatus {
    // `AXIsProcessTrusted()` is a synchronous FFI call into the
    // Accessibility/TCC subsystem; bound it so a wedged WindowServer can't
    // pin the runtime.
    let probe = run_blocking_with_timeout(
        "macos_accessibility_probe",
        PROBE_TIMEOUT,
        accessibility_trusted,
    )
    .await;
    accessibility_row(probe, ctx)
}

/// Map an `AXIsProcessTrusted()` probe outcome plus the persisted prompt
/// history onto the Accessibility row. Kept free of FFI so every
/// combination is testable regardless of the host's TCC state.
fn accessibility_row(
    probe: std::result::Result<bool, BlockingError>,
    ctx: &PermissionCheckContext,
) -> PermissionStatus {
    match probe {
        // A timed-out probe can't be read as either Granted or Denied, so
        // surface a degraded row (the Setup card still deep-links the user
        // into System Settings) rather than a misleading "granted".
        Err(err) => PermissionStatus {
            kind: PermissionKind::Accessibility,
            state: PermissionState::Denied,
            message: Some(format!(
                "Accessibility probe did not complete ({})",
                err.describe()
            )),
            reason_code: Some("probe_timed_out".to_owned()),
            setup_route: Some(ACCESSIBILITY_SETUP_ROUTE.to_owned()),
            docs_url: None,
        },
        Ok(true) => granted_accessibility_row(),
        Ok(false) if ctx.accessibility_prompted_at.is_none() => PermissionStatus {
            kind: PermissionKind::Accessibility,
            state: PermissionState::NotDetermined,
            message: Some("Accessibility not prompted".to_owned()),
            reason_code: Some("accessibility_not_prompted".to_owned()),
            setup_route: Some(ACCESSIBILITY_SETUP_ROUTE.to_owned()),
            docs_url: None,
        },
        Ok(false) => denied_accessibility_row(),
    }
}

/// Clipboard row of the permission report. macOS doesn't gate the pasteboard
/// via TCC, but `Clipboard::new()` returns `Err` in some sandboxed setups,
/// which is a useful real signal. The probe is bounded so a stuck pasteboard
/// server degrades the row instead of hanging the report.
async fn clipboard_status() -> PermissionStatus {
    match run_blocking_with_timeout("macos_clipboard_probe", PROBE_TIMEOUT, clipboard_probe).await {
        Ok(Ok(())) => PermissionStatus {
            kind: PermissionKind::Clipboard,
            state: PermissionState::Granted,
            message: None,
            reason_code: None,
            setup_route: None,
            docs_url: None,
        },
        Ok(Err(message)) => PermissionStatus {
            kind: PermissionKind::Clipboard,
            state: PermissionState::Denied,
            message: Some(message),
            reason_code: Some("clipboard_init_failed".to_owned()),
            setup_route: None,
            docs_url: None,
        },
        Err(err) => PermissionStatus {
            kind: PermissionKind::Clipboard,
            state: PermissionState::Denied,
            message: Some(format!(
                "clipboard probe did not complete ({})",
                err.describe()
            )),
            reason_code: Some("probe_timed_out".to_owned()),
            setup_route: None,
            docs_url: None,
        },
    }
}

fn clipboard_probe() -> std::result::Result<(), String> {
    arboard::Clipboard::new()
        .map(|_| ())
        .map_err(|err| err.to_string())
}

#[cfg(target_os = "macos")]
fn accessibility_trusted() -> bool {
    unsafe { ffi::AXIsProcessTrusted() }
}

#[cfg(not(target_os = "macos"))]
const fn accessibility_trusted() -> bool {
    false
}

/// Invoke `AXIsProcessTrustedWithOptions(kAXTrustedCheckOptionPrompt: prompt)`.
///
/// The macOS API does not change trust state synchronously: passing
/// `prompt = true` asks TCC to surface its dialog (or open the
/// Accessibility pane) on the next event-loop pass, and the function
/// returns the *current* trust value — typically `false` the first time
/// it is called. The runtime persists `accessibility_prompted_at`
/// independently so subsequent probes can tell `NotDetermined` from
/// `Denied`.
#[cfg(target_os = "macos")]
fn accessibility_trusted_with_prompt(prompt: bool) -> bool {
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;

    let key = unsafe { CFString::wrap_under_get_rule(ffi::kAXTrustedCheckOptionPrompt) };
    let value = CFBoolean::from(prompt);
    let options = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), value.as_CFType())]);
    unsafe { ffi::AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef().cast()) }
}

#[cfg(not(target_os = "macos"))]
const fn accessibility_trusted_with_prompt(_prompt: bool) -> bool {
    false
}

#[cfg(target_os = "macos")]
mod ffi {
    use core_foundation::dictionary::CFDictionaryRef;
    use core_foundation::string::CFStringRef;

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        pub fn AXIsProcessTrusted() -> bool;
        pub fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
        pub static kAXTrustedCheckOptionPrompt: CFStringRef;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::OffsetDateTime;

    fn timeout() -> BlockingError {
        BlockingError::Timeout {
            op: "macos_accessibility_probe",
            limit: PROBE_TIMEOUT,
        }
    }

    #[test]
    fn accessibility_row_combines_probe_result_with_prompt_history() {
        let never = PermissionCheckContext::default();
        let prompted = PermissionCheckContext {
            accessibility_prompted_at: Some(OffsetDateTime::UNIX_EPOCH),
        };
        let route = Some(ACCESSIBILITY_SETUP_ROUTE);
        let cases = [
            (Ok(true), &never, PermissionState::Granted, None, None),
            (Ok(true), &prompted, PermissionState::Granted, None, None),
            (
                Ok(false),
                &never,
                PermissionState::NotDetermined,
                Some("accessibility_not_prompted"),
                route,
            ),
            (
                Ok(false),
                &prompted,
                PermissionState::Denied,
                Some("accessibility_not_trusted"),
                route,
            ),
            (
                Err(timeout()),
                &never,
                PermissionState::Denied,
                Some("probe_timed_out"),
                route,
            ),
            (
                Err(timeout()),
                &prompted,
                PermissionState::Denied,
                Some("probe_timed_out"),
                route,
            ),
        ];
        for (probe, ctx, state, reason, setup_route) in cases {
            let label = format!("{probe:?} / prompted={:?}", ctx.accessibility_prompted_at);
            let row = accessibility_row(probe, ctx);
            assert_eq!(row.kind, PermissionKind::Accessibility, "{label}");
            assert_eq!(row.state, state, "{label}");
            assert_eq!(row.reason_code.as_deref(), reason, "{label}");
            assert_eq!(row.setup_route.as_deref(), setup_route, "{label}");
        }
    }

    #[test]
    fn accessibility_request_row_reports_grant_denial_and_timeout() {
        let granted = accessibility_request_row(Ok(true));
        assert_eq!(granted.state, PermissionState::Granted);
        assert_eq!(granted.reason_code, None);

        let denied = accessibility_request_row(Ok(false));
        assert_eq!(denied.state, PermissionState::Denied);
        assert_eq!(
            denied.reason_code.as_deref(),
            Some("accessibility_not_trusted")
        );
        assert_eq!(
            denied.setup_route.as_deref(),
            Some(ACCESSIBILITY_SETUP_ROUTE)
        );

        let timed_out = accessibility_request_row(Err(timeout()));
        assert_eq!(timed_out.state, PermissionState::Denied);
        assert_eq!(timed_out.reason_code.as_deref(), Some("probe_timed_out"));
        assert!(
            timed_out
                .message
                .as_deref()
                .is_some_and(|m| m.contains("did not return within")),
            "timeout row should carry the probe detail: {:?}",
            timed_out.message
        );
    }

    #[tokio::test]
    async fn check_reports_every_permission_kind_in_order() {
        // The live probes decide the Clipboard / Accessibility states, which
        // depend on the host; the row set and the unprobed rows do not.
        let statuses = MacosPermissionChecker
            .check(&PermissionCheckContext::default())
            .await
            .expect("check ok");
        let kinds: Vec<_> = statuses.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            [
                PermissionKind::Clipboard,
                PermissionKind::Accessibility,
                PermissionKind::InputMonitoring,
                PermissionKind::Notifications,
                PermissionKind::AutoLaunch,
            ]
        );
        assert!(
            statuses[2..]
                .iter()
                .all(|s| s.state == PermissionState::Unsupported)
        );
    }
}
