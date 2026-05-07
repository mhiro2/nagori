use async_trait::async_trait;
use nagori_core::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Hotkey {
    pub modifiers: Vec<HotkeyModifier>,
    pub key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HotkeyModifier {
    Command,
    Control,
    Option,
    Shift,
    Alt,
    Super,
}

#[async_trait]
pub trait HotkeyManager: Send + Sync {
    async fn register(&self, hotkey: Hotkey) -> Result<()>;
    async fn unregister(&self, hotkey: Hotkey) -> Result<()>;
}
