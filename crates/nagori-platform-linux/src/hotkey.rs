use async_trait::async_trait;
use nagori_core::{AppError, Result};
use nagori_platform::{Hotkey, HotkeyManager};

/// Daemon-side stub: real Linux/Wayland hotkeys go through the Tauri shell.
///
/// Wayland has no portable in-process global-hotkey API; the desktop
/// app relies on the `tauri-plugin-global-shortcut` plugin which talks
/// to the compositor via XDG portals or the `org.gnome.Shell`-style
/// session bus. Returning `Unsupported` here mirrors the macOS /
/// Windows arrangement and keeps a daemon-side caller from silently
/// duplicating the shell's registration.
#[derive(Debug, Default)]
pub struct LinuxHotkeyManager;

#[async_trait]
impl HotkeyManager for LinuxHotkeyManager {
    async fn register(&self, _hotkey: Hotkey) -> Result<()> {
        Err(AppError::Unsupported(
            "global hotkey registration is provided by the Tauri shell on Linux".to_owned(),
        ))
    }

    async fn unregister(&self, _hotkey: Hotkey) -> Result<()> {
        Ok(())
    }
}
