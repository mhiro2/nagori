//! Static capability report for the Windows host adapter.
//!
//! Windows is supported: capture covers text, file lists, and images
//! (encoded as `image/png` from `CF_DIBV5`/`CF_DIB`); copy-back covers
//! text, file lists, images, and multi-representation Preserve publishes
//! (`CF_UNICODETEXT` + `CF_HTML` + `Rich Text Format` + `CF_DIBV5` /
//! registered `PNG` + `CF_HDROP` in a single transaction); auto-paste /
//! global hotkeys / frontmost app are all wired; the release workflow
//! ships an unsigned NSIS bundle that the in-app updater can swap in
//! place (signing is still pending — `SmartScreen` warns on first
//! launch). There is no first-class permission UI: Windows does not
//! gate clipboard / input synthesis behind a user-managed permission.

use nagori_platform::{Capability, Platform, PlatformCapabilities, SupportTier};

#[must_use]
pub fn report_capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        platform: Platform::Windows,
        tier: SupportTier::Supported,
        capture_text: Capability::Available,
        capture_image: Capability::Available,
        // Windows captures Explorer file selections via CF_HDROP — the
        // README's "Text + files" coverage hinges on this row.
        capture_files: Capability::Available,
        write_text: Capability::Available,
        write_image: Capability::Available,
        // Multi-rep publishes CF_UNICODETEXT + CF_HTML + RFT + CF_DIBV5
        // (plus the registered "PNG" companion) + CF_HDROP in a single
        // OpenClipboard / EmptyClipboard / N × SetClipboardData
        // transaction, so Preserve can keep every stored representation
        // alive on copy-back without the primary-only fallback.
        clipboard_multi_representation_write: Capability::Available,
        auto_paste: Capability::Available,
        global_hotkey: Capability::Available,
        frontmost_app: Capability::Available,
        permissions_ui: Capability::Unsupported {
            reason: "Windows does not gate clipboard / input synthesis behind \
                 a user-managed permission UI; the doctor probe is a no-op."
                .to_owned(),
        },
        // release.yaml ships a signed NSIS bundle and a `latest.json`
        // entry alongside the macOS feed, so `tauri-plugin-updater` can
        // both probe availability and replace the installed binary in
        // place. Authenticode signing is still pending — `SmartScreen`
        // warns on first launch but the updater itself is functional.
        update_check: Capability::Available,
        // Windows has no cross-application overlay preview API; the
        // closest equivalents (Shell `IPreviewHandler`, PowerToys Peek)
        // are not standard surfaces we can rely on, so the palette
        // suppresses the Cmd+Y shortcut here.
        preview_quick_look: Capability::Unsupported {
            reason: "Windows has no OS-provided Quick-Look-equivalent overlay; \
                 the palette's preview shortcut is disabled."
                .to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_advertises_windows_supported_tier() {
        let caps = report_capabilities();
        assert_eq!(caps.platform, Platform::Windows);
        assert_eq!(caps.tier, SupportTier::Supported);
    }

    #[test]
    fn text_files_input_image_and_updater_rows_are_usable() {
        let caps = report_capabilities();
        assert!(caps.capture_text.is_usable());
        // Without `capture_files`, the README's file-list coverage on
        // Windows would silently regress. Lock it down here.
        assert!(caps.capture_files.is_usable());
        assert!(caps.capture_image.is_usable());
        assert!(caps.write_text.is_usable());
        assert!(caps.write_image.is_usable());
        // Multi-rep publish landed alongside CF_DIBV5 + CF_HTML +
        // CF_HDROP support; the README's Preserve row hinges on this
        // capability advertising as usable.
        assert!(caps.clipboard_multi_representation_write.is_usable());
        assert!(caps.auto_paste.is_usable());
        assert!(caps.global_hotkey.is_usable());
        assert!(caps.frontmost_app.is_usable());
        // Updater opened up alongside the NSIS release bundle — the
        // signed `latest.json` lets the in-app probe run on Windows.
        assert!(caps.update_check.is_usable());
    }

    #[test]
    fn permissions_ui_is_not_usable() {
        let caps = report_capabilities();
        assert!(!caps.permissions_ui.is_usable());
        assert!(matches!(
            caps.permissions_ui,
            Capability::Unsupported { .. }
        ));
    }

    #[test]
    fn preview_quick_look_is_unsupported() {
        let caps = report_capabilities();
        assert!(!caps.preview_quick_look.is_usable());
        assert!(matches!(
            caps.preview_quick_look,
            Capability::Unsupported { .. }
        ));
    }
}
