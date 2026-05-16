// macOS platform integration relies on Apple's Objective-C / Core Foundation
// FFI surface. Calls into objc2 / objc2-* / Core Foundation are inherently
// `unsafe` even with the safe wrappers, so override the workspace lint here.
#![allow(unsafe_code)]

mod capability;
mod clipboard;
mod hotkey;
mod paste;
mod permissions;
#[cfg(target_os = "macos")]
mod window;

pub use capability::report_capabilities;
pub use clipboard::MacosClipboard;
pub use hotkey::MacosHotkeyManager;
pub use paste::MacosPasteController;
pub use permissions::MacosPermissionChecker;
#[cfg(target_os = "macos")]
pub use window::MacosWindowBehavior;

#[cfg(not(target_os = "macos"))]
pub use stub_window::MacosWindowBehavior;

#[cfg(not(target_os = "macos"))]
mod stub_window {
    use async_trait::async_trait;
    use nagori_core::{AppError, Result};
    use nagori_platform::{FrontmostApp, WindowBehavior};

    #[derive(Debug, Default)]
    pub struct MacosWindowBehavior;

    impl MacosWindowBehavior {
        #[must_use]
        pub const fn new() -> Self {
            Self
        }
    }

    #[async_trait]
    impl WindowBehavior for MacosWindowBehavior {
        async fn frontmost_app(&self) -> Result<Option<FrontmostApp>> {
            Err(AppError::Unsupported(
                "frontmost_app is only supported on macOS".to_owned(),
            ))
        }
        async fn show_palette(&self) -> Result<()> {
            Ok(())
        }
        async fn hide_palette(&self) -> Result<()> {
            Ok(())
        }
    }
}
