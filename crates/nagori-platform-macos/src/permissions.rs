use async_trait::async_trait;
use nagori_core::Result;
use nagori_platform::{PermissionChecker, PermissionKind, PermissionState, PermissionStatus};

#[derive(Debug, Default)]
pub struct MacosPermissionChecker;

#[async_trait]
impl PermissionChecker for MacosPermissionChecker {
    async fn check(&self) -> Result<Vec<PermissionStatus>> {
        let accessibility = if accessibility_trusted() {
            PermissionStatus {
                kind: PermissionKind::Accessibility,
                state: PermissionState::Granted,
                message: None,
            }
        } else {
            PermissionStatus {
                kind: PermissionKind::Accessibility,
                state: PermissionState::Denied,
                message: Some(
                    "auto-paste requires Accessibility permission. Open System \
                     Settings → Privacy & Security → Accessibility and enable \
                     nagori."
                        .to_owned(),
                ),
            }
        };
        // Clipboard: probe arboard. macOS doesn't actually gate the
        // pasteboard via TCC, but `Clipboard::new()` returns Err in some
        // sandboxed setups, which is a useful real signal.
        let clipboard = match clipboard_probe() {
            Ok(()) => PermissionStatus {
                kind: PermissionKind::Clipboard,
                state: PermissionState::Granted,
                message: None,
            },
            Err(message) => PermissionStatus {
                kind: PermissionKind::Clipboard,
                state: PermissionState::Denied,
                message: Some(message),
            },
        };
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
                    "InputMonitoring status cannot be probed without TCC \
                     entitlements; check System Settings → Privacy & \
                     Security → Input Monitoring manually."
                        .to_owned(),
                ),
            },
            PermissionStatus {
                kind: PermissionKind::Notifications,
                state: PermissionState::Unsupported,
                message: Some(
                    "Notification authorization is not probed; nagori does \
                     not currently dispatch notifications."
                        .to_owned(),
                ),
            },
            PermissionStatus {
                kind: PermissionKind::AutoLaunch,
                state: PermissionState::Unsupported,
                message: Some(
                    "AutoLaunch state is managed by tauri-plugin-autostart \
                     and is not probed at the daemon layer."
                        .to_owned(),
                ),
            },
        ])
    }

    async fn request(&self, permission: PermissionKind) -> Result<PermissionStatus> {
        match permission {
            PermissionKind::Accessibility => {
                let granted = accessibility_trusted();
                Ok(PermissionStatus {
                    kind: permission,
                    state: if granted {
                        PermissionState::Granted
                    } else {
                        PermissionState::Denied
                    },
                    message: if granted {
                        None
                    } else {
                        Some(
                            "open macOS System Settings → Privacy & Security \
                             → Accessibility to grant this permission"
                                .to_owned(),
                        )
                    },
                })
            }
            other => Ok(PermissionStatus {
                kind: other,
                state: PermissionState::Unsupported,
                message: Some(
                    "this permission cannot be requested programmatically \
                     from the daemon; manage it in System Settings."
                        .to_owned(),
                ),
            }),
        }
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

#[cfg(target_os = "macos")]
mod ffi {
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        pub fn AXIsProcessTrusted() -> bool;
    }
}
