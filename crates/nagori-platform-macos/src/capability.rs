//! Static capability report for the macOS host adapter.
//!
//! Mirrors the README support matrix: macOS is first-class, with image
//! clipboard read/write, frontmost-app probing, the Accessibility /
//! Clipboard permission UI, and the bundled update-check probe all
//! wired. `auto_paste` is the one row that is not flat `Available` —
//! it requires the Accessibility permission. `MacosPermissionChecker`
//! reports the live grant state on its own channel; this layer only
//! states "the OS could do it, given a permission".

use nagori_platform::{Capability, PermissionKind, Platform, PlatformCapabilities, SupportTier};

#[must_use]
pub fn report_capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        platform: Platform::MacOS,
        tier: SupportTier::Supported,
        capture_text: Capability::Available,
        capture_image: Capability::Available,
        // macOS clipboard captures NSPasteboard file URLs as file-list
        // payloads (alongside text and images), matching the README's
        // first-class capture coverage.
        capture_files: Capability::Available,
        write_text: Capability::Available,
        write_image: Capability::Available,
        auto_paste: Capability::RequiresPermission {
            permission: PermissionKind::Accessibility,
            message: "auto-paste requires the Accessibility permission. Open \
                 System Settings → Privacy & Security → Accessibility and \
                 enable nagori."
                .to_owned(),
        },
        global_hotkey: Capability::Available,
        frontmost_app: Capability::Available,
        permissions_ui: Capability::Available,
        update_check: Capability::Available,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_advertises_macos_supported_tier() {
        let caps = report_capabilities();
        assert_eq!(caps.platform, Platform::MacOS);
        assert_eq!(caps.tier, SupportTier::Supported);
    }

    #[test]
    fn first_class_rows_are_available_and_usable() {
        let caps = report_capabilities();
        for cap in [
            &caps.capture_text,
            &caps.capture_image,
            &caps.capture_files,
            &caps.write_text,
            &caps.write_image,
            &caps.global_hotkey,
            &caps.frontmost_app,
            &caps.permissions_ui,
            &caps.update_check,
        ] {
            assert_eq!(cap, &Capability::Available);
            assert!(cap.is_usable());
        }
    }

    #[test]
    fn auto_paste_requires_accessibility_and_is_not_usable() {
        let caps = report_capabilities();
        match &caps.auto_paste {
            Capability::RequiresPermission { permission, .. } => {
                assert_eq!(*permission, PermissionKind::Accessibility);
            }
            other => panic!("expected RequiresPermission, got {other:?}"),
        }
        // RequiresPermission must not be reported as usable — the UI
        // should drive the user through the permission flow first.
        assert!(!caps.auto_paste.is_usable());
    }
}
