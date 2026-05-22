// Windows platform integration calls into Win32 via the `windows-sys` crate.
// Even the safe re-exports surface `unsafe` because the underlying API is
// raw FFI, so the workspace-wide `unsafe_code = "deny"` lint is overridden
// here exactly as it is for the macOS adapter.
#![allow(unsafe_code)]

mod capability;
mod clipboard;
mod hotkey;
mod paste;
mod permissions;
mod window;

pub use capability::report_capabilities;
pub use clipboard::WindowsClipboard;
pub use hotkey::WindowsHotkeyManager;
pub use paste::WindowsPasteController;
pub use permissions::WindowsPermissionChecker;
pub use window::WindowsWindowBehavior;

// Windows has no OS-provided Quick-Look-equivalent overlay (`IPreviewHandler`
// is a Shell preview-pane COM surface, not a cross-app overlay; PowerToys
// Peek is third-party). Alias the shared `UnsupportedPreviewController` so
// the platform-native wiring can pick a concrete type per OS without each
// stub crate re-implementing the trait.
pub type WindowsPreviewController = nagori_platform::UnsupportedPreviewController;
