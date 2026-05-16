//! Linux platform adapter.
//!
//! Linux support targets **Wayland** sessions only. X11 sessions are
//! intentionally rejected at adapter construction so the daemon never
//! attempts to read or synthesise input through an X11 backend — that
//! path was excluded from MVP scope and shipping a half-tested X11
//! fallback would silently regress the privacy posture we ship on the
//! other platforms (no AX-equivalent guard, no secure-focus handling).
//!
//! The crate compiles on every host so workspace `cargo check` from
//! macOS / Windows still succeeds; non-Linux targets get inert stubs
//! that return `Unsupported`.

mod capability;
mod clipboard;
mod hotkey;
mod paste;
mod permissions;
mod window;

pub use capability::report_capabilities;
pub use clipboard::LinuxClipboard;
pub use hotkey::LinuxHotkeyManager;
pub use paste::LinuxPasteController;
pub use permissions::LinuxPermissionChecker;
pub use window::LinuxWindowBehavior;
