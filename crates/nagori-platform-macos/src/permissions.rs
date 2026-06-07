use async_trait::async_trait;
use nagori_core::Result;
use nagori_platform::{
    PermissionCheckContext, PermissionChecker, PermissionKind, PermissionState, PermissionStatus,
    run_blocking_with_timeout,
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
        let granted = accessibility_trusted_with_prompt(prompt);
        Ok(if granted {
            PermissionStatus {
                kind: PermissionKind::Accessibility,
                state: PermissionState::Granted,
                message: None,
                reason_code: None,
                setup_route: None,
                docs_url: None,
            }
        } else {
            // `prompt = true` and `granted = false` means either:
            //   (a) the TCC dialog is now showing and the user has not
            //       responded yet, or
            //   (b) TCC has already recorded a Deny / unknown identity for
            //       this bundle so the dialog was suppressed.
            // Either way the actionable next step is the Setup card
            // (which deep-links into System Settings); the message stays
            // a terse CLI-friendly summary.
            PermissionStatus {
                kind: PermissionKind::Accessibility,
                state: PermissionState::Denied,
                message: Some("Accessibility not trusted".to_owned()),
                reason_code: Some("accessibility_not_trusted".to_owned()),
                setup_route: Some(ACCESSIBILITY_SETUP_ROUTE.to_owned()),
                docs_url: None,
            }
        })
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
    // pin the runtime. A timed-out probe can't be read as either Granted or
    // Denied, so surface a degraded row (the Setup card still deep-links the
    // user into System Settings) rather than a misleading "granted".
    let trusted = match run_blocking_with_timeout(
        "macos_accessibility_probe",
        PROBE_TIMEOUT,
        accessibility_trusted,
    )
    .await
    {
        Ok(value) => value,
        Err(err) => {
            return PermissionStatus {
                kind: PermissionKind::Accessibility,
                state: PermissionState::Denied,
                message: Some(format!(
                    "Accessibility probe did not complete ({})",
                    err.describe()
                )),
                reason_code: Some("probe_timed_out".to_owned()),
                setup_route: Some(ACCESSIBILITY_SETUP_ROUTE.to_owned()),
                docs_url: None,
            };
        }
    };
    if trusted {
        return PermissionStatus {
            kind: PermissionKind::Accessibility,
            state: PermissionState::Granted,
            message: None,
            reason_code: None,
            setup_route: None,
            docs_url: None,
        };
    }
    if ctx.accessibility_prompted_at.is_none() {
        return PermissionStatus {
            kind: PermissionKind::Accessibility,
            state: PermissionState::NotDetermined,
            message: Some("Accessibility not prompted".to_owned()),
            reason_code: Some("accessibility_not_prompted".to_owned()),
            setup_route: Some(ACCESSIBILITY_SETUP_ROUTE.to_owned()),
            docs_url: None,
        };
    }
    PermissionStatus {
        kind: PermissionKind::Accessibility,
        state: PermissionState::Denied,
        message: Some("Accessibility not trusted".to_owned()),
        reason_code: Some("accessibility_not_trusted".to_owned()),
        setup_route: Some(ACCESSIBILITY_SETUP_ROUTE.to_owned()),
        docs_url: None,
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

    fn find_accessibility(statuses: &[PermissionStatus]) -> &PermissionStatus {
        statuses
            .iter()
            .find(|s| s.kind == PermissionKind::Accessibility)
            .expect("checker reports an Accessibility row")
    }

    #[tokio::test]
    async fn check_reports_not_determined_without_prompt_history() {
        // This test only proves the discrimination logic — on macOS CI
        // the `AXIsProcessTrusted()` call returns `false` (no signed
        // bundle), so we can assert the NotDetermined path. On non-macOS
        // hosts the stub also returns `false`, hitting the same branch.
        let checker = MacosPermissionChecker;
        let ctx = PermissionCheckContext::default();
        let statuses = checker.check(&ctx).await.expect("check ok");
        let accessibility = find_accessibility(&statuses);
        // `accessibility_status` probes `AXIsProcessTrusted()` under a 2 s
        // timeout. Running under `cargo test` without a WindowServer session
        // the FFI call can exceed that, and the production path then reports a
        // degraded `Denied` (reason `probe_timed_out`). That bypasses the
        // prompt-history discrimination this test covers, so skip rather than
        // fail — the environment couldn't answer, which is not a logic bug.
        if accessibility.state == PermissionState::Denied
            && accessibility.reason_code.as_deref() == Some("probe_timed_out")
        {
            eprintln!("skipping: accessibility probe timed out in this environment");
            return;
        }
        // We can't force `AXIsProcessTrusted()` to a specific value
        // here, but the test environment never has it granted (no
        // signed bundle on CI), so NotDetermined is the expected
        // outcome when prompt history is empty.
        assert!(
            matches!(
                accessibility.state,
                PermissionState::NotDetermined | PermissionState::Granted
            ),
            "expected NotDetermined (no history) or Granted (locally-trusted dev shell), got {:?}",
            accessibility.state,
        );
        if accessibility.state == PermissionState::NotDetermined {
            assert_eq!(
                accessibility.reason_code.as_deref(),
                Some("accessibility_not_prompted")
            );
            assert_eq!(
                accessibility.setup_route.as_deref(),
                Some("setup/accessibility")
            );
        }
    }

    #[tokio::test]
    async fn check_reports_denied_when_prompt_history_present() {
        let checker = MacosPermissionChecker;
        let ctx = PermissionCheckContext {
            accessibility_prompted_at: Some(OffsetDateTime::UNIX_EPOCH),
        };
        let statuses = checker.check(&ctx).await.expect("check ok");
        let accessibility = find_accessibility(&statuses);
        // Same caveat as above: on a granted dev shell this returns
        // Granted, but the only "Denied vs NotDetermined" discrimination
        // we want to cover here flows through the same branch — once
        // prompted, NotDetermined must not appear.
        assert!(
            !matches!(accessibility.state, PermissionState::NotDetermined),
            "prompt history should rule out NotDetermined, got {:?}",
            accessibility.state,
        );
    }
}
