//! Static capability report for the Linux Wayland host adapter.
//!
//! Linux Wayland is **experimental**: text / image / file-list capture
//! and text + image copy-back work on compositors that expose
//! `wlr_data_control` / `ext_data_control`, but the adapter refuses to
//! start on X11 and on compositors without a data-control manager
//! (notably GNOME Wayland), global hotkeys / frontmost-app probing have
//! no portable Wayland API and are surfaced as `Unsupported`, and
//! auto-paste is off by default because the compositor cannot confirm
//! which surface receives the keystroke (`LinuxAutoPaste`; opt in with
//! `NAGORI_LINUX_AUTO_PASTE=1`, which then also requires the external
//! `wtype` binary). Those gaps — no X11 backend, no GNOME path, no
//! Wayland global shortcut, unverifiable paste target — are why the
//! tier is `Experimental` rather than `Supported`: the core "hotkey →
//! pick → paste into the previous window" flow does not hold on every
//! Linux desktop the way it does on macOS / Windows. The permission UI is a no-op probe (no TCC-style
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

use nagori_platform::{
    Capability, NO_AI_ENGINE_REASON, Platform, PlatformCapabilities, SupportTier,
};

use crate::paste::LinuxAutoPaste;

/// Capability report for the running process, with the auto-paste row
/// following the `NAGORI_LINUX_AUTO_PASTE` opt-in.
#[must_use]
pub fn report_capabilities() -> PlatformCapabilities {
    report_capabilities_for(LinuxAutoPaste::from_env())
}

/// [`report_capabilities`] for an explicit auto-paste mode, so the desktop
/// / CLI surfaces and the paste controller can never disagree about whether
/// synthetic paste is on.
#[must_use]
pub fn report_capabilities_for(auto_paste: LinuxAutoPaste) -> PlatformCapabilities {
    PlatformCapabilities {
        platform: Platform::LinuxWayland,
        tier: SupportTier::Experimental,
        capture_text: Capability::Available,
        capture_image: Capability::Available,
        capture_files: Capability::Available,
        write_text: Capability::Available,
        write_image: Capability::Available,
        clipboard_multi_representation_write: Capability::Available,
        auto_paste: match auto_paste {
            LinuxAutoPaste::Disabled => Capability::Unsupported {
                reason: LinuxAutoPaste::disabled_reason(),
            },
            LinuxAutoPaste::Unverified => Capability::RequiresExternalTool {
                tool: "wtype".to_owned(),
                install_hint: Some(
                    "install the `wtype` package (e.g. `apt install wtype` or \
                     `pacman -S wtype`); the compositor must also expose \
                     zwp_virtual_keyboard_v1."
                        .to_owned(),
                ),
            },
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
        // No on-device AI backend on Linux yet — `default_ai_engine`
        // wires `None` here, so model-backed AI actions are refused and
        // the desktop hides the AI surfaces. Lights up automatically once
        // a provider (e.g. OpenAI-compatible) is wired.
        ai_actions: Capability::Unsupported {
            reason: NO_AI_ENGINE_REASON.to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_advertises_linux_wayland_experimental_tier() {
        // X11 / GNOME Wayland cannot start the adapter, pure Wayland has no
        // global hotkey, and the paste target is unverifiable — the report
        // must not claim first-class support.
        let caps = report_capabilities_for(LinuxAutoPaste::Disabled);
        assert_eq!(caps.platform, Platform::LinuxWayland);
        assert_eq!(caps.tier, SupportTier::Experimental);
    }

    #[test]
    fn auto_paste_is_unsupported_until_the_session_opts_in() {
        let caps = report_capabilities_for(LinuxAutoPaste::Disabled);
        match &caps.auto_paste {
            Capability::Unsupported { reason } => {
                assert!(reason.contains("NAGORI_LINUX_AUTO_PASTE"), "{reason}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
        assert!(!caps.auto_paste.is_supported_by_platform());
    }

    #[test]
    fn text_rows_are_usable() {
        let caps = report_capabilities();
        assert!(caps.capture_text.is_usable());
        assert!(caps.write_text.is_usable());
    }

    #[test]
    fn opted_in_auto_paste_requires_wtype() {
        let caps = report_capabilities_for(LinuxAutoPaste::Unverified);
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
        assert!(caps.auto_paste.is_supported_by_platform());
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
