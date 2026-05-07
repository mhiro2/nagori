use async_trait::async_trait;
use nagori_core::{AppError, Result};
use nagori_platform::{Hotkey, HotkeyManager};

#[derive(Debug, Default)]
pub struct MacosHotkeyManager;

#[async_trait]
impl HotkeyManager for MacosHotkeyManager {
    async fn register(&self, _hotkey: Hotkey) -> Result<()> {
        Err(AppError::Unsupported(
            "global hotkey registration is provided by the Tauri shell in MVP".to_owned(),
        ))
    }

    async fn unregister(&self, _hotkey: Hotkey) -> Result<()> {
        Ok(())
    }
}
