//! Static capability report for the Linux Wayland host adapter.
//!
//! Linux Wayland is experimental: text capture and copy-back work on
//! compositors that expose `wlr_data_control` / `ext_data_control`,
//! auto-paste is conditional on the external `wtype` binary, and
//! global hotkeys / frontmost-app probing have no portable Wayland
//! API and are surfaced as `Unsupported`. The permission UI is a
//! no-op probe (no TCC-style gate on Wayland) and the release feed
//! does not ship an updater channel for the Linux tarball.
//!
//! Scope: this report describes the **Wayland** Linux target nagori
//! builds for. X11 sessions and Wayland compositors without a
//! `data_control` manager (notably GNOME) are not represented by a
//! separate capability row — the runtime rejects them at adapter
//! startup (`LinuxClipboard::new()` returns `AppError::Unsupported`
//! with a Wayland-specific hint, see `nagori-platform-native`). The
//! capability layer is intentionally static and only answers "could
//! this feature work on a supported Linux Wayland session"; live
//! compositor probes stay in the runtime path so the two channels
//! don't disagree on a flaky compositor.

use nagori_platform::{Capability, Platform, PlatformCapabilities, SupportTier};

#[must_use]
pub fn report_capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        platform: Platform::LinuxWayland,
        tier: SupportTier::Experimental,
        capture_text: Capability::Available,
        capture_image: Capability::Unsupported {
            reason: "Linux Wayland clipboard capture is text-only; image \
                 payloads are not implemented yet."
                .to_owned(),
        },
        capture_files: Capability::Unsupported {
            reason: "Linux Wayland clipboard capture is text-only; file-list \
                 payloads are not modelled (no `text/uri-list` integration \
                 yet)."
                .to_owned(),
        },
        write_text: Capability::Available,
        write_image: Capability::Unsupported {
            reason: "Linux Wayland copy-back is text-only; image entries \
                 from macOS sessions cannot be written back."
                .to_owned(),
        },
        clipboard_multi_representation_write: Capability::Unsupported {
            reason: "wl-clipboard-rs publishes a single MIME per offer; \
                 Wayland copy-back falls back to the primary representation \
                 via write_text and cannot re-publish HTML/RTF alongside it."
                .to_owned(),
        },
        auto_paste: Capability::RequiresExternalTool {
            tool: "wtype".to_owned(),
            install_hint: Some(
                "install the `wtype` package (e.g. `apt install wtype` or \
                 `pacman -S wtype`); the compositor must also expose \
                 zwp_virtual_keyboard_v1."
                    .to_owned(),
            ),
        },
        global_hotkey: Capability::Unsupported {
            reason: "tauri-plugin-global-shortcut is X11-only upstream; pure \
                 Wayland sessions cannot register an in-app global hotkey."
                .to_owned(),
        },
        frontmost_app: Capability::Unsupported {
            reason: "Wayland has no portable API to identify the frontmost \
                 client; frontmost_app() returns None."
                .to_owned(),
        },
        permissions_ui: Capability::Unsupported {
            reason: "Wayland sessions do not gate clipboard / input synthesis \
                 behind a user-managed permission UI; the doctor probe is a \
                 no-op."
                .to_owned(),
        },
        update_check: Capability::Unsupported {
            reason: "no Linux updater feed is published; the tarball ships \
                 without in-app update notifications."
                .to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_advertises_linux_wayland_experimental_tier() {
        let caps = report_capabilities();
        assert_eq!(caps.platform, Platform::LinuxWayland);
        assert_eq!(caps.tier, SupportTier::Experimental);
    }

    #[test]
    fn text_rows_are_usable() {
        let caps = report_capabilities();
        assert!(caps.capture_text.is_usable());
        assert!(caps.write_text.is_usable());
    }

    #[test]
    fn auto_paste_requires_wtype() {
        let caps = report_capabilities();
        match &caps.auto_paste {
            Capability::RequiresExternalTool { tool, install_hint } => {
                assert_eq!(tool, "wtype");
                // Install hint must be populated — a None would force the
                // UI to invent its own copy, which would diverge from the
                // README troubleshooting guide.
                assert!(install_hint.is_some());
            }
            other => panic!("expected RequiresExternalTool, got {other:?}"),
        }
        assert!(!caps.auto_paste.is_usable());
    }

    #[test]
    fn image_hotkey_frontmost_and_updater_are_not_usable() {
        let caps = report_capabilities();
        for cap in [
            &caps.capture_image,
            &caps.capture_files,
            &caps.write_image,
            &caps.clipboard_multi_representation_write,
            &caps.global_hotkey,
            &caps.frontmost_app,
            &caps.permissions_ui,
            &caps.update_check,
        ] {
            assert!(!cap.is_usable());
            assert!(matches!(cap, Capability::Unsupported { .. }));
        }
    }
}
