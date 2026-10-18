use async_trait::async_trait;
use nagori_core::Result;
use serde::{Deserialize, Serialize};

use crate::RestoreTarget;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasteResult {
    pub pasted: bool,
    pub message: Option<String>,
}

#[async_trait]
pub trait PasteController: Send + Sync {
    async fn paste_frontmost(&self) -> Result<PasteResult>;

    /// [`Self::paste_frontmost`], synthesising only if `target` is still the
    /// foreground surface.
    ///
    /// A front-end that hands focus back to the source app and then waits
    /// before pasting cannot stop the user (or another app) from moving focus
    /// during the wait, and the keystroke goes to whatever is in front. The
    /// platform adapters that capture a restore target (macOS: bundle id;
    /// Windows: HWND + owning PID) compare it with the foreground inside the
    /// synthesis call itself, right before the key events are posted, and
    /// refuse with [`PasteFailureReason::PreviousAppLost`] on a mismatch.
    /// The default ignores `target`: platforms without one (Linux Wayland, the
    /// no-op controller) record no restore target to compare against.
    ///
    /// [`PasteFailureReason::PreviousAppLost`]: nagori_core::PasteFailureReason::PreviousAppLost
    async fn paste_frontmost_into(&self, target: Option<&RestoreTarget>) -> Result<PasteResult> {
        let _ = target;
        self.paste_frontmost().await
    }
}

#[derive(Debug, Default)]
pub struct NoopPasteController;

#[async_trait]
impl PasteController for NoopPasteController {
    async fn paste_frontmost(&self) -> Result<PasteResult> {
        Ok(PasteResult {
            pasted: false,
            message: Some("auto-paste is not enabled on this platform".to_owned()),
        })
    }
}
