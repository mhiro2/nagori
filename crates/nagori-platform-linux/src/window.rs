use async_trait::async_trait;
use nagori_core::Result;
use nagori_platform::{FrontmostApp, WindowBehavior};

/// Linux/Wayland window-behaviour adapter.
///
/// Wayland intentionally does not expose a portable "frontmost app"
/// query — the protocol treats per-surface focus as the compositor's
/// private business, and the existing protocol extensions that do
/// expose it (e.g. `zwlr_foreign_toplevel_management_v1`,
/// `ext_foreign_toplevel_list_v1`) are compositor-specific and not
/// widely implemented. Returning `Ok(None)` here is the documented
/// contract for "no source attribution available" and matches the way
/// the capture loop already handles a missing frontmost on macOS when
/// AX is revoked.
///
/// `frontmost_focused_is_secure` keeps the trait default (`Ok(false)`):
/// Wayland has no equivalent of `kAXSecureTextField`, so the
/// password-input guard relies on the `SensitivityClassifier` detectors
/// (PEM blocks, password-manager source apps) rather than a structural
/// AX probe.
#[derive(Debug, Default)]
pub struct LinuxWindowBehavior;

impl LinuxWindowBehavior {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl WindowBehavior for LinuxWindowBehavior {
    async fn frontmost_app(&self) -> Result<Option<FrontmostApp>> {
        Ok(None)
    }

    async fn show_palette(&self) -> Result<()> {
        Ok(())
    }

    async fn hide_palette(&self) -> Result<()> {
        Ok(())
    }
}
