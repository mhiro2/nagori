//! OS-native preview surface (macOS Quick Look).
//!
//! The palette's Cmd+Y shortcut calls into this trait to pop up a
//! native preview of the selected clipboard entry — most usefully an
//! image or a file in a `FileList` entry. Only macOS exposes a
//! cross-application overlay API (Quick Look); Windows and Linux have
//! no equivalent, so their adapters return [`nagori_core::AppError::Unsupported`]
//! and the palette suppresses the shortcut via the capability row.
//!
//! The trait deliberately takes a file path (or set of paths) rather
//! than raw bytes — Quick Look itself is built around file URLs, and
//! pushing temp-file materialisation up to the caller keeps this crate
//! free of `std::fs` writes or content-type sniffing. The desktop shell
//! resolves an entry to a temp file (image bytes → `.png`/`.jpeg`, plain
//! text → `.txt`) or a list of pre-existing paths (`FileList` content)
//! before calling [`PreviewController::preview`].
//!
//! Multiple items render as Quick Look's index-bar navigation; passing
//! an empty slice is treated as a no-op error so the platform layer
//! never has to guess the user's intent.

use std::path::PathBuf;

use async_trait::async_trait;
use nagori_core::{AppError, Result};

/// A single file the OS preview surface should display.
///
/// Carries an absolute path because Quick Look (and any future
/// equivalent on other platforms) resolves previews through file URLs.
/// The struct is intentionally minimal — callers that need a richer
/// hint (UTI, suggested display name) should write the file with the
/// appropriate extension and let the OS infer the rest from the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewItem {
    pub path: PathBuf,
}

impl PreviewItem {
    #[must_use]
    pub const fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

#[async_trait]
pub trait PreviewController: Send + Sync {
    /// Present a native preview overlay for the given items.
    ///
    /// On macOS this maps to Quick Look (the same panel Finder opens
    /// when the user presses space). Implementations should return as
    /// soon as the preview is queued — the call does not block until
    /// the panel is dismissed.
    ///
    /// `items` must be non-empty; an empty slice returns
    /// [`AppError::InvalidInput`].
    async fn preview(&self, items: &[PreviewItem]) -> Result<()>;
}

/// Fallback used on platforms without an OS-native preview surface
/// (Windows, Linux Wayland) and in tests that don't drive the real
/// adapter.
///
/// Returns [`AppError::Unsupported`] for every call so the desktop
/// shell can light up the same "feature isn't available on this
/// platform" path it uses for auto-paste on hosts without `wtype`.
#[derive(Debug, Default)]
pub struct UnsupportedPreviewController;

#[async_trait]
impl PreviewController for UnsupportedPreviewController {
    async fn preview(&self, _items: &[PreviewItem]) -> Result<()> {
        Err(AppError::Unsupported(
            "native preview (Quick Look) is only available on macOS".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unsupported_controller_returns_unsupported() {
        let controller = UnsupportedPreviewController;
        let item = PreviewItem::new(PathBuf::from("/tmp/example.png"));
        match controller.preview(&[item]).await {
            Err(AppError::Unsupported(_)) => {}
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn preview_item_roundtrips_path() {
        let path = PathBuf::from("/var/tmp/preview.txt");
        let item = PreviewItem::new(path.clone());
        assert_eq!(item.path, path);
    }
}
