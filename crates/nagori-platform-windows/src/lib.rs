// Windows platform integration calls into Win32 via the `windows-sys` crate.
// Even the safe re-exports surface `unsafe` because the underlying API is
// raw FFI, so the workspace-wide `unsafe_code = "deny"` lint is overridden
// here exactly as it is for the macOS adapter.
#![allow(unsafe_code)]

mod clipboard;
mod hotkey;
mod paste;
mod permissions;
mod window;

pub use clipboard::WindowsClipboard;
pub use hotkey::WindowsHotkeyManager;
pub use paste::WindowsPasteController;
pub use permissions::WindowsPermissionChecker;
pub use window::WindowsWindowBehavior;
