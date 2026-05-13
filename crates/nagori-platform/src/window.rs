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
    /// so password keystrokes never reach history. macOS-only; other
    /// platforms default to `Ok(false)` until per-OS support lands.
    async fn frontmost_focused_is_secure(&self) -> Result<bool> {
        Ok(false)
    }
}
