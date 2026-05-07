use async_trait::async_trait;
use nagori_core::{Result, SourceApp};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontmostApp {
    pub source: SourceApp,
    pub window_title: Option<String>,
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
}
