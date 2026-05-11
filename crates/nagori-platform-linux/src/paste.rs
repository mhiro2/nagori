use async_trait::async_trait;
use nagori_core::{AppError, Result};
use nagori_platform::{PasteController, PasteResult};

/// Synthesize Ctrl+V into the frontmost Wayland surface via `wtype`.
///
/// Wayland has no portable in-process input-synthesis API: there is no
/// equivalent of `CGEventPost` or `SendInput`. The de-facto tool is
/// `wtype`, a small CLI that talks to `zwp_virtual_keyboard_v1` on
/// compositors that expose it (Sway, KDE, Hyprland, river). Shelling
/// out keeps the daemon free of compositor-specific protocol code at
/// the cost of one process spawn per paste — acceptable because paste
/// is a user-initiated event, not a hot path.
///
/// If `wtype` is not on `$PATH` (or refuses to run because the
/// compositor doesn't expose the virtual-keyboard protocol) we surface
/// the error as `AppError::Platform` so the desktop falls back to
/// copy-only behaviour, matching the macOS / Windows "Accessibility
/// missing" semantics.
#[derive(Debug, Default)]
pub struct LinuxPasteController;

#[async_trait]
impl PasteController for LinuxPasteController {
    async fn paste_frontmost(&self) -> Result<PasteResult> {
        #[cfg(target_os = "linux")]
        {
            // Run on the blocking pool for symmetry with the macOS /
            // Windows adapters — a misbehaving compositor can keep
            // wtype waiting on a virtual-keyboard handshake for tens of
            // ms and we don't want that to pin a tokio worker.
            let output = tokio::process::Command::new("wtype")
                .arg("-M")
                .arg("ctrl")
                .arg("v")
                .arg("-m")
                .arg("ctrl")
                .output()
                .await
                .map_err(|err| {
                    AppError::Platform(format!(
                        "auto-paste failed: could not invoke `wtype` ({err}). Install the \
                         `wtype` package and ensure the compositor exposes \
                         zwp_virtual_keyboard_v1.",
                    ))
                })?;
            if output.status.success() {
                Ok(PasteResult {
                    pasted: true,
                    message: None,
                })
            } else {
                // `wtype` writes diagnostics to stderr. Surface them so
                // the doctor / toast layer can show the actual reason
                // (e.g. "compositor does not support the virtual
                // keyboard protocol") without us having to enumerate
                // every variant.
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(AppError::Platform(format!(
                    "auto-paste failed: wtype exited with {} ({}).",
                    output.status,
                    stderr.trim(),
                )))
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(AppError::Unsupported(
                "Linux auto-paste is only available on Linux".to_owned(),
            ))
        }
    }
}
