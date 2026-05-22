//! Static capability report for the Linux Wayland host adapter.
//!
//! Linux Wayland is supported: text / image / file-list capture and
//! text + image copy-back work on compositors that expose
//! `wlr_data_control` / `ext_data_control`, auto-paste is conditional
//! on the external `wtype` binary, and global hotkeys / frontmost-app
//! probing have no portable Wayland API and are surfaced as
//! `Unsupported`. The permission UI is a no-op probe (no TCC-style
//! gate on Wayland). The release feed publishes a `latest.json` entry
//! for Linux too — availability check runs everywhere, and the
//! updater plugin can swap an `AppImage` install in place; `deb`
//! installs see the availability surface but follow the GitHub
//! release link to upgrade manually (no dpkg root prompt).
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
        tier: SupportTier::Supported,
        capture_text: Capability::Available,
        capture_image: Capability::Available,
        capture_files: Capability::Available,
        write_text: Capability::Available,
        write_image: Capability::Available,
        clipboard_multi_representation_write: Capability::Available,
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
        // release.yaml ships a `deb` + `AppImage` pair and the signed
        // `latest.json` advertises both, so the availability probe runs
        // on every Linux install. Whether the discovered update can be
        // applied in place is decided per medium at runtime (AppImage
        // only — `deb` users follow the GitHub release link).
        update_check: Capability::Available,
        // Linux has no DE-agnostic Quick Look equivalent — `gnome-sushi`
        // is GNOME-only and KDE preview hooks live behind `kio`. The
        // palette suppresses the Cmd+Y shortcut here rather than
        // ship an inconsistent per-DE fallback.
        preview_quick_look: Capability::Unsupported {
            reason: "Linux Wayland has no DE-agnostic Quick-Look-equivalent \
                 overlay; the palette's preview shortcut is disabled."
                .to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_advertises_linux_wayland_supported_tier() {
        let caps = report_capabilities();
        assert_eq!(caps.platform, Platform::LinuxWayland);
        assert_eq!(caps.tier, SupportTier::Supported);
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
    fn image_and_file_capture_rows_are_usable() {
        let caps = report_capabilities();
        assert!(caps.capture_image.is_usable());
        assert!(caps.capture_files.is_usable());
        assert!(caps.write_image.is_usable());
    }

    #[test]
    fn multi_rep_write_is_usable() {
        let caps = report_capabilities();
        assert!(caps.clipboard_multi_representation_write.is_usable());
    }

    #[test]
    fn hotkey_frontmost_and_permissions_ui_are_not_usable() {
        let caps = report_capabilities();
        for cap in [
            &caps.global_hotkey,
            &caps.frontmost_app,
            &caps.permissions_ui,
            &caps.preview_quick_look,
        ] {
            assert!(!cap.is_usable());
            assert!(matches!(cap, Capability::Unsupported { .. }));
        }
    }

    #[test]
    fn update_check_is_usable() {
        // release.yaml publishes deb + AppImage and `latest.json` lists
        // both, so the availability probe runs on every Linux install.
        // In-place apply is gated per install medium in the desktop
        // shell (`download_supported`).
        let caps = report_capabilities();
        assert!(caps.update_check.is_usable());
    }
}
