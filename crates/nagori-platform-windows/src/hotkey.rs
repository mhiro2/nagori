use async_trait::async_trait;
use nagori_core::{AppError, Result};
use nagori_platform::{Hotkey, HotkeyManager};

/// Daemon-side stub: real Windows hotkeys go through the Tauri shell.
///
/// The desktop app uses the `global-shortcut` plugin which calls Win32
/// `RegisterHotKey` under the hood; this adapter therefore intentionally
/// returns `Unsupported` so attempting to register from inside the daemon
/// fails loudly instead of silently duplicating the shell's registration.
/// Mirrors the MVP arrangement on macOS.
#[derive(Debug, Default)]
pub struct WindowsHotkeyManager;

#[async_trait]
impl HotkeyManager for WindowsHotkeyManager {
    async fn register(&self, _hotkey: Hotkey) -> Result<()> {
        Err(AppError::Unsupported(
            "global hotkey registration is provided by the Tauri shell on Windows".to_owned(),
        ))
    }

    async fn unregister(&self, _hotkey: Hotkey) -> Result<()> {
        Ok(())
    }
}
