use async_trait::async_trait;
use nagori_core::Result;
#[cfg(target_os = "linux")]
use nagori_platform::run_blocking_with_timeout;
use nagori_platform::{
    PermissionCheckContext, PermissionChecker, PermissionKind, PermissionState, PermissionStatus,
};
#[cfg(target_os = "linux")]
use std::time::Duration;
#[cfg(target_os = "linux")]
use wl_clipboard_rs::paste;

/// Upper bound on how long a synchronous permission probe may block the async
/// runtime. `get_mime_types` does a Wayland protocol round-trip and `wtype
/// --help` spawns a subprocess; a wedged compositor or a hung `wtype` would
/// otherwise pin the tokio worker for the whole `nagori doctor` / onboarding /
/// Settings call. Mirrors the clipboard adapter's `PIPE_READ_TIMEOUT`.
#[cfg(target_os = "linux")]
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

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
    async fn check(&self, _ctx: &PermissionCheckContext) -> Result<Vec<PermissionStatus>> {
        Ok(vec![
            check_clipboard().await,
            check_accessibility().await,
            PermissionStatus {
                kind: PermissionKind::InputMonitoring,
                state: PermissionState::Unsupported,
                message: Some("input monitoring permission is not modelled on Linux".to_owned()),
                reason_code: None,
                setup_route: None,
                docs_url: None,
            },
            PermissionStatus {
                kind: PermissionKind::Notifications,
                state: PermissionState::Unsupported,
                message: Some(
                    "notification authorization is brokered by the desktop environment".to_owned(),
                ),
                reason_code: None,
                setup_route: None,
                docs_url: None,
            },
            PermissionStatus {
                kind: PermissionKind::AutoLaunch,
                state: PermissionState::Unsupported,
                message: Some(
                    "auto-launch is managed by tauri-plugin-autostart on Linux".to_owned(),
                ),
                reason_code: None,
                setup_route: None,
                docs_url: None,
            },
        ])
    }

    async fn request_accessibility(&self, _prompt: bool) -> Result<PermissionStatus> {
        // Linux Wayland gates auto-paste on `wtype` + a compositor that
        // exposes `zwp_virtual_keyboard_v1`. Neither lives in a settings
        // pane that we can deep-link into, so the equivalent of macOS's
        // "request the OS prompt" is to re-run the same probe `check`
        // uses (binary on PATH) and report what we find. The Setup card
        // renders the install hint when this comes back Denied.
        Ok(check_accessibility().await)
    }
}

#[cfg(target_os = "linux")]
async fn check_clipboard() -> PermissionStatus {
    // `get_mime_types` does a synchronous Wayland protocol round-trip; a
    // wedged compositor would otherwise pin the tokio worker for the whole
    // report. Bound it on the blocking pool the same way the clipboard
    // adapter bounds its own ops, degrading to a `probe_timed_out` row on
    // overrun instead of hanging the doctor / onboarding call.
    match run_blocking_with_timeout("linux_clipboard_probe", PROBE_TIMEOUT, probe_clipboard_mime)
        .await
    {
        Ok(status) => status,
        Err(err) => PermissionStatus {
            kind: PermissionKind::Clipboard,
            state: PermissionState::Denied,
            message: Some(format!(
                "Wayland clipboard probe did not complete ({}).",
                err.describe()
            )),
            reason_code: Some("probe_timed_out".to_owned()),
            setup_route: None,
            docs_url: None,
        },
    }
}

#[cfg(target_os = "linux")]
fn probe_clipboard_mime() -> PermissionStatus {
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
            reason_code: None,
            setup_route: None,
            docs_url: None,
        },
        Err(paste::Error::MissingProtocol { name, version }) => PermissionStatus {
            kind: PermissionKind::Clipboard,
            state: PermissionState::Denied,
            message: Some(format!(
                "compositor does not expose {name} v{version}. Nagori requires wlr-data-control \
                 or ext-data-control (Sway, KDE Plasma 5.27+, Hyprland, river)."
            )),
            reason_code: Some("clipboard_missing_protocol".to_owned()),
            setup_route: None,
            docs_url: None,
        },
        Err(paste::Error::WaylandConnection(err)) => PermissionStatus {
            kind: PermissionKind::Clipboard,
            state: PermissionState::Denied,
            message: Some(format!(
                "could not connect to a Wayland compositor ({err}). Linux nagori requires a \
                 Wayland session; X11 is not supported."
            )),
            reason_code: Some("clipboard_no_wayland".to_owned()),
            setup_route: None,
            docs_url: None,
        },
        Err(err) => PermissionStatus {
            kind: PermissionKind::Clipboard,
            state: PermissionState::Denied,
            message: Some(format!("could not bind Wayland clipboard ({err}).")),
            reason_code: Some("clipboard_bind_failed".to_owned()),
            setup_route: None,
            docs_url: None,
        },
    }
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::unused_async)] // await lives behind the linux cfg branch
async fn check_clipboard() -> PermissionStatus {
    PermissionStatus {
        kind: PermissionKind::Clipboard,
        state: PermissionState::Unsupported,
        message: Some("LinuxPermissionChecker is only meaningful on Linux".to_owned()),
        reason_code: None,
        setup_route: None,
        docs_url: None,
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
        // Bound the subprocess probe: a hung `wtype` (or a host under heavy
        // fork pressure) must not pin the runtime for the whole report.
        // `kill_on_drop` reaps the child if the deadline elapses and the
        // future is dropped.
        let probe = tokio::process::Command::new("wtype")
            .arg("--help")
            .kill_on_drop(true)
            .output();
        match tokio::time::timeout(PROBE_TIMEOUT, probe).await {
            Ok(Ok(_)) => PermissionStatus {
                kind: PermissionKind::Accessibility,
                state: PermissionState::Granted,
                message: Some(
                    "wtype is available; auto-paste also requires the compositor to expose \
                     zwp_virtual_keyboard_v1 (Sway, KDE Plasma 5.27+, Hyprland, river)."
                        .to_owned(),
                ),
                reason_code: None,
                setup_route: None,
                docs_url: None,
            },
            Ok(Err(err)) => PermissionStatus {
                kind: PermissionKind::Accessibility,
                state: PermissionState::Denied,
                message: Some(format!(
                    "wtype was not found on PATH ({err}); auto-paste will fall back to \
                     copy-only. Install the `wtype` package to enable Ctrl+V synthesis."
                )),
                reason_code: Some("accessibility_wtype_missing".to_owned()),
                setup_route: Some("setup/accessibility".to_owned()),
                docs_url: None,
            },
            Err(_elapsed) => PermissionStatus {
                kind: PermissionKind::Accessibility,
                state: PermissionState::Denied,
                message: Some(
                    "checking for wtype timed out; auto-paste will fall back to copy-only."
                        .to_owned(),
                ),
                reason_code: Some("probe_timed_out".to_owned()),
                setup_route: Some("setup/accessibility".to_owned()),
                docs_url: None,
            },
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        PermissionStatus {
            kind: PermissionKind::Accessibility,
            state: PermissionState::Unsupported,
            message: Some("LinuxPermissionChecker is only meaningful on Linux".to_owned()),
            reason_code: None,
            setup_route: None,
            docs_url: None,
        }
    }
}
