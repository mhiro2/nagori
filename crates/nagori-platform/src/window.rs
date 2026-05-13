use async_trait::async_trait;
use nagori_core::{Result, SourceApp};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontmostApp {
    pub source: SourceApp,
    pub window_title: Option<String>,
}

/// Snapshot the desktop shell can later use to restore foreground focus.
///
/// Carried as a dedicated type so we can attach a platform-specific
/// opaque handle (Windows HWND) without polluting the cross-platform
/// [`FrontmostApp`] reporting shape used by the capture loop / IPC
/// layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreTarget {
    pub source: SourceApp,
    /// Opaque platform handle.
    /// - macOS: always `None` — restore uses [`SourceApp::bundle_id`].
    /// - Windows: `Some(hwnd as usize as u64)` — the foreground window
    ///   handle at palette open. `SetForegroundWindow` accepts it
    ///   directly so we avoid the bundle-id mismatch that previously
    ///   left auto-paste landing in Nagori's own webview.
    /// - Linux Wayland: always `None`.
    pub native_handle: Option<u64>,
}

#[async_trait]
pub trait WindowBehavior: Send + Sync {
    async fn frontmost_app(&self) -> Result<Option<FrontmostApp>>;
    async fn show_palette(&self) -> Result<()>;
    async fn hide_palette(&self) -> Result<()>;
    /// Activate (focus) the app identified by `bundle_id`. Used after
    /// hiding the palette so a subsequent ⌘V lands in the user's
    /// previous frontmost app instead of the (now-hidden) `WebView`.
    /// Default: no-op so non-macOS targets remain unaffected.
    async fn activate_app(&self, _bundle_id: &str) -> Result<()> {
        Ok(())
    }
    /// Restore foreground focus using a platform-specific
    /// [`RestoreTarget`] snapshot taken before the palette opened. The
    /// default impl falls back to [`Self::activate_app`] with the
    /// `bundle_id` from the source — sufficient on macOS, no-op on
    /// platforms without a bundle id. Windows overrides this to use the
    /// HWND so restore actually targets the original window even when
    /// the same executable has several top-level windows open.
    async fn activate_restore_target(&self, target: &RestoreTarget) -> Result<()> {
        if let Some(bundle_id) = target.source.bundle_id.as_deref() {
            self.activate_app(bundle_id).await
        } else {
            Ok(())
        }
    }
    /// Reports whether the frontmost app's currently-focused UI element
    /// is a secure text field (i.e. a password input). The capture loop
    /// uses this signal to suppress the next clip before classification
    /// so password keystrokes never reach history.
    ///
    /// **Capability: macOS-only in the current implementation.** macOS
    /// exposes the answer through the Accessibility API's
    /// `kAXSecureTextField` role, and the macOS adapter overrides this
    /// default with a real probe. Windows has UI Automation
    /// (`IUIAutomation::GetFocusedElement` + `IsPasswordProperty`) that
    /// could in principle answer the same question, but the current
    /// Win32-based `WindowsWindowBehavior` does not consume it; until
    /// that lands, the capability is effectively unavailable. Linux
    /// Wayland goes further and structurally withholds per-surface focus
    /// from non-compositor clients, so an equivalent probe has no
    /// portable path at all.
    ///
    /// On non-macOS targets the capture loop therefore falls back to
    /// downstream defences: the `SensitivityClassifier` content detectors
    /// (PEM blocks, JWTs), the user-configurable secret regex denylist,
    /// and — *on Windows only* — the password-manager source-app
    /// denylist driven by `frontmost_app()`. The denylist is not
    /// available on Linux Wayland because `LinuxWindowBehavior` returns
    /// `Ok(None)` from `frontmost_app()`, so Linux relies solely on the
    /// content-level guards.
    ///
    /// Returning `Ok(false)` rather than `Err(Unsupported)` is
    /// intentional: the capture loop's "fail-closed when AX errors
    /// persist" path is macOS-specific and a platform that never had AX
    /// should not trip it.
    async fn frontmost_focused_is_secure(&self) -> Result<bool> {
        Ok(false)
    }
}
