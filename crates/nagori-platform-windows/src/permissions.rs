use async_trait::async_trait;
use nagori_core::Result;
use nagori_platform::{
    PermissionCheckContext, PermissionChecker, PermissionKind, PermissionState, PermissionStatus,
};

/// Reports `Granted` for kinds that Windows doesn't gate behind user prompts.
///
/// Windows does not gate clipboard reads, synthesised input, or autostart
/// behind TCC-style user permissions, so most kinds report `Granted` once
/// the basic clipboard probe succeeds. The `Unsupported` slots mirror the
/// macOS adapter so the doctor / onboarding UI renders consistent rows
/// without inventing a Windows-specific permission taxonomy.
#[derive(Debug, Default)]
pub struct WindowsPermissionChecker;

#[async_trait]
impl PermissionChecker for WindowsPermissionChecker {
    async fn check(&self, _ctx: &PermissionCheckContext) -> Result<Vec<PermissionStatus>> {
        let clipboard = match arboard::Clipboard::new() {
            Ok(_) => PermissionStatus {
                kind: PermissionKind::Clipboard,
                state: PermissionState::Granted,
                message: None,
                reason_code: None,
                setup_route: None,
                docs_url: None,
            },
            Err(err) => PermissionStatus {
                kind: PermissionKind::Clipboard,
                state: PermissionState::Denied,
                message: Some(err.to_string()),
                reason_code: Some("clipboard_init_failed".to_owned()),
                setup_route: None,
                docs_url: None,
            },
        };
        Ok(vec![
            clipboard,
            // Accessibility on macOS gates `SendInput`-equivalent input
            // synthesis; on Windows the closest analogue is UIPI, which is
            // implicit and not user-toggleable. Report `Granted` so the UI
            // doesn't surface a permission that the user cannot manage, but
            // include the UIPI caveat: a non-elevated daemon cannot inject
            // input into a UAC-elevated foreground window, so an unexpected
            // "Ctrl+V did nothing" with an elevated app on top is a UIPI
            // artefact, not a bug in our paste pipeline.
            PermissionStatus {
                kind: PermissionKind::Accessibility,
                state: PermissionState::Granted,
                message: Some(
                    "SendInput cannot reach UAC-elevated foreground windows from a non-elevated \
                     daemon (UIPI)."
                        .to_owned(),
                ),
                reason_code: None,
                setup_route: None,
                docs_url: None,
            },
            PermissionStatus {
                kind: PermissionKind::InputMonitoring,
                state: PermissionState::Unsupported,
                message: Some("input monitoring permission is not modelled on Windows".to_owned()),
                reason_code: None,
                setup_route: None,
                docs_url: None,
            },
            PermissionStatus {
                kind: PermissionKind::Notifications,
                state: PermissionState::Unsupported,
                message: Some(
                    "notification authorization is managed by Windows Action Center".to_owned(),
                ),
                reason_code: None,
                setup_route: None,
                docs_url: None,
            },
            PermissionStatus {
                kind: PermissionKind::AutoLaunch,
                state: PermissionState::Unsupported,
                message: Some(
                    "auto-launch is managed by tauri-plugin-autostart on Windows".to_owned(),
                ),
                reason_code: None,
                setup_route: None,
                docs_url: None,
            },
        ])
    }

    async fn request_accessibility(&self, _prompt: bool) -> Result<PermissionStatus> {
        // Windows has no TCC-style permission gating `SendInput`; the
        // closest analogue (UIPI) is implicit and cannot be requested.
        // Return the same `Granted` row the regular check emits so the
        // frontend can render a uniform "you're good" state.
        Ok(PermissionStatus {
            kind: PermissionKind::Accessibility,
            state: PermissionState::Granted,
            message: Some(
                "Windows has no Accessibility-style permission; if SendInput is dropped, check \
                 whether the target window belongs to an elevated process (UIPI)."
                    .to_owned(),
            ),
            reason_code: None,
            setup_route: None,
            docs_url: None,
        })
    }
}
