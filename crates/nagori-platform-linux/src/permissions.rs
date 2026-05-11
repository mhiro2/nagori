use async_trait::async_trait;
use nagori_core::Result;
use nagori_platform::{PermissionChecker, PermissionKind, PermissionState, PermissionStatus};
#[cfg(target_os = "linux")]
use wl_clipboard_rs::paste;

/// Linux permission probe.
///
/// Reports `Granted` for kinds that Linux/Wayland doesn't gate behind
/// user prompts, plus a synthetic `Accessibility` row tied to `wtype`
/// availability so the onboarding banner and `nagori doctor` can flag
/// "auto-paste won't work" the same way they do on macOS.
///
/// The clipboard probe calls the same `wl-clipboard-rs` entry point the
/// capture loop uses, so a failure here is the most reliable single
/// signal that the daemon would also fail at capture time; it doubles
/// as the install validation step. No X11 fallback is exercised because
/// `wl-clipboard-rs` does not implement one.
#[derive(Debug, Default)]
pub struct LinuxPermissionChecker;

#[async_trait]
impl PermissionChecker for LinuxPermissionChecker {
    async fn check(&self) -> Result<Vec<PermissionStatus>> {
        Ok(vec![
            check_clipboard(),
            check_accessibility().await,
            PermissionStatus {
                kind: PermissionKind::InputMonitoring,
                state: PermissionState::Unsupported,
                message: Some("input monitoring permission is not modelled on Linux".to_owned()),
            },
            PermissionStatus {
                kind: PermissionKind::Notifications,
                state: PermissionState::Unsupported,
                message: Some(
                    "notification authorization is brokered by the desktop environment".to_owned(),
                ),
            },
            PermissionStatus {
                kind: PermissionKind::AutoLaunch,
                state: PermissionState::Unsupported,
                message: Some(
                    "auto-launch is managed by tauri-plugin-autostart on Linux".to_owned(),
                ),
            },
        ])
    }

    async fn request(&self, permission: PermissionKind) -> Result<PermissionStatus> {
        Ok(PermissionStatus {
            kind: permission,
            state: PermissionState::Unsupported,
            message: Some(
                "this permission cannot be requested programmatically on Linux".to_owned(),
            ),
        })
    }
}

#[cfg(target_os = "linux")]
fn check_clipboard() -> PermissionStatus {
    // Probe the same Wayland-only path the capture loop uses, so the
    // doctor surfaces the actionable error when the compositor lacks
    // wlr-data-control / ext-data-control (GNOME's current default).
    // The connection failure itself (`WaylandConnection`) is the
    // signal for "no Wayland session". `WAYLAND_SOCKET` sessions are
    // not honoured here because `wayland-client` consumes the
    // inherited fd on first connect, so the probe would burn it
    // before the daemon's capture loop could reuse it.
    match paste::get_mime_types(paste::ClipboardType::Regular, paste::Seat::Unspecified) {
        // Either a populated selection or an empty-but-bound one is a
        // successful protocol bind — both are `Granted`.
        Ok(_) | Err(paste::Error::ClipboardEmpty | paste::Error::NoSeats) => PermissionStatus {
            kind: PermissionKind::Clipboard,
            state: PermissionState::Granted,
            message: None,
        },
        Err(paste::Error::MissingProtocol { name, version }) => PermissionStatus {
            kind: PermissionKind::Clipboard,
            state: PermissionState::Denied,
            message: Some(format!(
                "compositor does not expose {name} v{version}. Nagori requires wlr-data-control \
                 or ext-data-control (Sway, KDE Plasma 5.27+, Hyprland, river)."
            )),
        },
        Err(paste::Error::WaylandConnection(err)) => PermissionStatus {
            kind: PermissionKind::Clipboard,
            state: PermissionState::Denied,
            message: Some(format!(
                "could not connect to a Wayland compositor ({err}). Linux nagori requires a \
                 Wayland session; X11 is not supported."
            )),
        },
        Err(err) => PermissionStatus {
            kind: PermissionKind::Clipboard,
            state: PermissionState::Denied,
            message: Some(format!("could not bind Wayland clipboard ({err}).")),
        },
    }
}

#[cfg(not(target_os = "linux"))]
fn check_clipboard() -> PermissionStatus {
    PermissionStatus {
        kind: PermissionKind::Clipboard,
        state: PermissionState::Unsupported,
        message: Some("LinuxPermissionChecker is only meaningful on Linux".to_owned()),
    }
}

#[allow(clippy::unused_async)] // await lives behind the linux cfg branch
async fn check_accessibility() -> PermissionStatus {
    // On macOS this row gates `CGEventPost`; on Linux the analogue is
    // `wtype` being installed *and* the compositor exposing
    // `zwp_virtual_keyboard_v1`. We can cheaply check the first
    // condition (binary on PATH) and leave the second to the actual
    // paste call — surfacing "wtype not found" as Denied gives the
    // user something to act on, while a Granted row means "we'll
    // attempt the paste; the compositor may still refuse it".
    #[cfg(target_os = "linux")]
    {
        match tokio::process::Command::new("wtype")
            .arg("--help")
            .output()
            .await
        {
            Ok(_) => PermissionStatus {
                kind: PermissionKind::Accessibility,
                state: PermissionState::Granted,
                message: Some(
                    "wtype is available; auto-paste also requires the compositor to expose \
                     zwp_virtual_keyboard_v1 (Sway, KDE Plasma 5.27+, Hyprland, river)."
                        .to_owned(),
                ),
            },
            Err(err) => PermissionStatus {
                kind: PermissionKind::Accessibility,
                state: PermissionState::Denied,
                message: Some(format!(
                    "wtype was not found on PATH ({err}); auto-paste will fall back to \
                     copy-only. Install the `wtype` package to enable Ctrl+V synthesis."
                )),
            },
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        PermissionStatus {
            kind: PermissionKind::Accessibility,
            state: PermissionState::Unsupported,
            message: Some("LinuxPermissionChecker is only meaningful on Linux".to_owned()),
        }
    }
}
