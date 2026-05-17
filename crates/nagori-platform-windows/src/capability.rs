//! Static capability report for the Windows host adapter.
//!
//! Windows is experimental: capture covers text, file lists, and images
//! (encoded as `image/png` from `CF_DIBV5`/`CF_DIB`); copy-back covers
//! text, file lists, images, and multi-representation Preserve publishes
//! (`CF_UNICODETEXT` + `CF_HTML` + `Rich Text Format` + `CF_DIBV5` /
//! registered `PNG` + `CF_HDROP` in a single transaction); auto-paste /
//! global hotkeys / frontmost app are all wired; there is no first-class
//! permission UI or bundled update-check probe yet (the release workflow
//! does not currently produce a signed Windows installer).

use nagori_platform::{Capability, Platform, PlatformCapabilities, SupportTier};

#[must_use]
pub fn report_capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        platform: Platform::Windows,
        tier: SupportTier::Experimental,
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
        update_check: Capability::Unsupported {
            reason: "no signed Windows release bundle is produced yet, so the \
                 updater feed is macOS-only."
                .to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_advertises_windows_experimental_tier() {
        let caps = report_capabilities();
        assert_eq!(caps.platform, Platform::Windows);
        assert_eq!(caps.tier, SupportTier::Experimental);
    }

    #[test]
    fn text_files_input_and_image_rows_are_usable() {
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
    }

    #[test]
    fn permissions_ui_and_updater_are_not_usable() {
        let caps = report_capabilities();
        for cap in [&caps.permissions_ui, &caps.update_check] {
            assert!(!cap.is_usable());
            assert!(matches!(cap, Capability::Unsupported { .. }));
        }
    }
}
