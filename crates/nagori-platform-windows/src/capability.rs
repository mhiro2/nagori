//! Static capability report for the Windows host adapter.
//!
//! Windows is experimental: capture and copy-back are text-and-files
//! only (no image clipboard), auto-paste / global hotkeys / frontmost
//! app are all wired, and there is no first-class permission UI or
//! bundled update-check probe yet (the release workflow does not
//! currently produce a signed Windows installer).

use nagori_platform::{Capability, Platform, PlatformCapabilities, SupportTier};

#[must_use]
pub fn report_capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        platform: Platform::Windows,
        tier: SupportTier::Experimental,
        capture_text: Capability::Available,
        capture_image: Capability::Unsupported {
            reason: "Windows capture is text + CF_HDROP file lists only; \
                 image clipboard capture is not implemented yet."
                .to_owned(),
        },
        // Windows captures Explorer file selections via CF_HDROP — the
        // README's "Text + files" coverage hinges on this row.
        capture_files: Capability::Available,
        write_text: Capability::Available,
        write_image: Capability::Unsupported {
            reason: "writing images back to the Windows clipboard is not \
                 implemented; image entries from macOS sessions fall back \
                 to Unsupported on copy-back."
                .to_owned(),
        },
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
    fn text_files_and_input_rows_are_usable() {
        let caps = report_capabilities();
        assert!(caps.capture_text.is_usable());
        // Without `capture_files`, the README's "Text + files" claim
        // would silently regress on Windows. Lock it down here.
        assert!(caps.capture_files.is_usable());
        assert!(caps.write_text.is_usable());
        assert!(caps.auto_paste.is_usable());
        assert!(caps.global_hotkey.is_usable());
        assert!(caps.frontmost_app.is_usable());
    }

    #[test]
    fn image_clipboard_and_updater_are_not_usable() {
        let caps = report_capabilities();
        for cap in [
            &caps.capture_image,
            &caps.write_image,
            &caps.permissions_ui,
            &caps.update_check,
        ] {
            assert!(!cap.is_usable());
            assert!(matches!(cap, Capability::Unsupported { .. }));
        }
    }
}
